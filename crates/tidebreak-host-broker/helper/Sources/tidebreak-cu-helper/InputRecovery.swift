import AppKit
import Foundation

/// Records synthetic downs before delivery so the broker can release them after
/// this process exits. Recovery never presses input or restores an old pointer.
enum InputRecovery {
    struct Held: Codable, Equatable {
        let kind: String
        let code: UInt16
    }

    struct Journal: Codable {
        let invocationId: String
        var held: [Held]
    }

    struct Result: Encodable {
        let released: Int
        let preservedPhysicalHolds: Int
    }

    private static func fail(_ message: String) -> HelperError {
        HelperError(code: .operationFailed, message: "Input recovery: \(message)")
    }

    static func checkCancellation(_ request: HelperRequest) throws {
        guard request.inputCancelPath != nil || request.inputInvocationId != nil else { return }
        guard let path = request.inputCancelPath, let invocation = request.inputInvocationId,
            let data = FileManager.default.contents(atPath: path), data.count <= 128,
            String(data: data, encoding: .utf8) == invocation
        else { throw HelperError(code: .yielded, message: "the helper invocation was cancelled") }
    }

    private static func journalURL(_ request: HelperRequest) throws -> URL {
        guard let path = request.inputJournalPath, path.hasPrefix("/"),
            let invocation = request.inputInvocationId, UUID(uuidString: invocation) != nil
        else { throw fail("a broker-owned journal is required before sending input") }
        let url = URL(fileURLWithPath: path)
        let manager = FileManager.default
        for (file, expectedType, expectedMode) in [
            (url, FileAttributeType.typeRegular, 0o600),
            (url.deletingLastPathComponent(), FileAttributeType.typeDirectory, 0o700),
        ] {
            let attributes = try manager.attributesOfItem(atPath: file.path)
            guard attributes[.type] as? FileAttributeType == expectedType,
                (attributes[.ownerAccountID] as? NSNumber)?.uint32Value == getuid(),
                (attributes[.posixPermissions] as? NSNumber)?.intValue == expectedMode
            else { throw fail("the journal must remain private and must not be a symbolic link") }
        }
        return url
    }

    static func load(_ request: HelperRequest) throws -> Journal {
        let url = try journalURL(request)
        let file = try FileHandle(forReadingFrom: url)
        defer { try? file.close() }
        let data = try file.read(upToCount: 16 * 1024 + 1) ?? Data()
        guard data.count <= 16 * 1024 else { throw fail("the journal exceeds its size limit") }
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let journal = try decoder.decode(Journal.self, from: data)
        guard journal.invocationId == request.inputInvocationId,
            journal.held.count <= 16,
            Set(journal.held.map { "\($0.kind):\($0.code)" }).count == journal.held.count,
            journal.held.allSatisfy({
                ($0.kind == "key" && $0.code <= 127) || ($0.kind == "mouse" && $0.code <= 1)
            })
        else { throw fail("the journal does not describe this invocation's input") }
        return journal
    }

    static func save(_ journal: Journal, request: HelperRequest) throws {
        let url = try journalURL(request)
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        let data = try encoder.encode(journal)
        guard data.count <= 16 * 1024 else { throw fail("the journal exceeds its size limit") }
        // Set permissions before publication. A crash must not leave a valid
        // held-input record that recovery rejects because chmod never ran.
        let temporary = url.deletingLastPathComponent()
            .appendingPathComponent(".input-" + UUID().uuidString)
        let descriptor = open(temporary.path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0o600)
        guard descriptor >= 0 else { throw fail("could not create a private journal update") }
        let file = FileHandle(fileDescriptor: descriptor, closeOnDealloc: true)
        defer {
            try? file.close()
            try? FileManager.default.removeItem(at: temporary)
        }
        try file.write(contentsOf: data)
        try file.close()
        guard rename(temporary.path, url.path) == 0 else {
            throw fail("could not publish the journal update")
        }
    }

    static func heldControl(_ event: CGEvent) -> (Held, Bool)? {
        switch event.type {
        case .leftMouseDown: return (Held(kind: "mouse", code: 0), true)
        case .leftMouseUp: return (Held(kind: "mouse", code: 0), false)
        case .rightMouseDown: return (Held(kind: "mouse", code: 1), true)
        case .rightMouseUp: return (Held(kind: "mouse", code: 1), false)
        case .flagsChanged:
            let code = UInt16(event.getIntegerValueField(.keyboardEventKeycode))
            let mask: CGEventFlags
            switch code {
            case 54, 55: mask = .maskCommand
            case 56, 60: mask = .maskShift
            case 58, 61: mask = .maskAlternate
            case 59, 62: mask = .maskControl
            case 63: mask = .maskSecondaryFn
            default: return nil
            }
            return (Held(kind: "key", code: code), event.flags.contains(mask))
        case .keyDown, .keyUp:
            return (
                Held(kind: "key", code: UInt16(event.getIntegerValueField(.keyboardEventKeycode))),
                event.type == .keyDown
            )
        default: return nil
        }
    }

    static func post(_ event: CGEvent, request: HelperRequest) throws {
        guard let (control, down) = heldControl(event) else {
            event.post(tap: .cghidEventTap)
            return
        }
        try track(
            control, down: down, request: request,
            deliver: {
                event.post(tap: .cghidEventTap)
            })
    }

    /// The record precedes a down. A matching up precedes removal of its record.
    /// A crash between either pair leaves only a conservative release to retry.
    static func track(_ control: Held, down: Bool, request: HelperRequest, deliver: () -> Void)
        throws
    {
        if down {
            try checkCancellation(request)
            var journal = try load(request)
            guard !journal.held.contains(control), journal.held.count < 16 else {
                throw fail("the invocation already holds this input")
            }
            journal.held.append(control)
            try save(journal, request: request)
            deliver()
        } else {
            // Release even if the journal has become unreadable. The broker
            // retains the record and reports an uncertain outcome on failure.
            deliver()
            var journal = try load(request)
            journal.held.removeAll { $0 == control }
            try save(journal, request: request)
        }
    }

    static func releaseRecorded(_ request: HelperRequest) throws -> Result {
        guard AXIsProcessTrusted() else { throw fail("Accessibility permission is unavailable") }
        let journal = try load(request)
        return try recover(
            journal: journal, request: request,
            physicallyHeld: { control in
                // Apple's CGEventSource.h defines HIDSystemState as hardware sources.
                // Our events use combinedSessionState, which includes session sources.
                if control.kind == "mouse" {
                    return CGEventSource.buttonState(
                        .hidSystemState, button: CGMouseButton(rawValue: UInt32(control.code))!)
                }
                return CGEventSource.keyState(.hidSystemState, key: CGKeyCode(control.code))
            },
            release: { control in
                guard let source = CGEventSource(stateID: .combinedSessionState) else {
                    throw fail("could not create a release event source")
                }
                let event: CGEvent?
                if control.kind == "mouse" {
                    guard let point = CGEvent(source: nil)?.location else {
                        throw fail("could not read the current pointer position")
                    }
                    event = CGEvent(
                        mouseEventSource: source,
                        mouseType: control.code == 0 ? .leftMouseUp : .rightMouseUp,
                        mouseCursorPosition: point, mouseButton: control.code == 0 ? .left : .right)
                } else {
                    event = CGEvent(
                        keyboardEventSource: source, virtualKey: CGKeyCode(control.code),
                        keyDown: false)
                }
                guard let event else { throw fail("could not create the recorded release") }
                event.flags = CGEventSource.flagsState(.hidSystemState)
                event.post(tap: .cghidEventTap)
            })
    }

    /// Reverse press order releases the primary key before its modifiers.
    /// Physical holds belong to the user and complete through their own up.
    static func recover(
        journal: Journal, request: HelperRequest, physicallyHeld: (Held) -> Bool,
        release: (Held) throws -> Void
    ) throws -> Result {
        var pending = journal
        var released = 0
        var preserved = 0
        for control in journal.held.reversed() {
            if physicallyHeld(control) {
                preserved += 1
            } else {
                try release(control)
                released += 1
            }
            pending.held.removeAll { $0 == control }
            try save(pending, request: request)
        }
        return Result(released: released, preservedPhysicalHolds: preserved)
    }
}
