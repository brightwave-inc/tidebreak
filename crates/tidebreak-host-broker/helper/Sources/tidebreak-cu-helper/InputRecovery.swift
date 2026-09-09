import AppKit
import Foundation

/// Records synthetic downs before delivery so the broker can release them after
/// this process exits. Recovery never presses input or restores an old pointer.
enum InputRecovery {
    struct Held: Codable, Equatable {
        let kind: String
        let code: UInt16
        var releaseText: String? = nil
        var releaseTextIgnoringModifiers: String? = nil
        var releaseFlags: UInt64? = nil
        var releaseX: Double? = nil
        var releaseY: Double? = nil
        var releaseLocalX: Double? = nil
        var releaseLocalY: Double? = nil
        var eventNumber: Int64? = nil
        var clickCount: Int64? = nil

        func matches(_ other: Held) -> Bool { kind == other.kind && code == other.code }
    }

    struct Journal: Codable {
        let invocationId: String
        var held: [Held]
        var target: TargetedInput.Target? = nil
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
                (($0.kind == "key" && $0.code <= 127) || ($0.kind == "mouse" && $0.code <= 1))
                    && ($0.releaseText?.utf16.count ?? 0) <= 16
                    && ($0.releaseTextIgnoringModifiers?.utf16.count ?? 0) <= 16
                    && ($0.releaseX?.isFinite ?? true) && ($0.releaseY?.isFinite ?? true)
                    && ($0.releaseLocalX?.isFinite ?? true) && ($0.releaseLocalY?.isFinite ?? true)
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

    static func post(
        _ event: CGEvent, request: HelperRequest,
        target: TargetedInput.Target? = nil,
        deliver: (CGEvent, pid_t) -> Void = { event, pid in event.postToPid(pid) }
    ) throws {
        guard let target else { throw fail("independent input requires a bound process") }
        guard let (identity, down) = heldControl(event) else {
            if event.type == .leftMouseDragged || event.type == .rightMouseDragged {
                try checkCancellation(request)
                var journal = try load(request)
                let code: UInt16 = event.type == .leftMouseDragged ? 0 : 1
                guard journal.target == target,
                    let index = journal.held.firstIndex(where: {
                        $0.kind == "mouse" && $0.code == code
                    })
                else { throw fail("the invocation does not hold this target's mouse button") }
                try recordMousePosition(event, control: &journal.held[index])
                try save(journal, request: request)
            }
            deliver(event, target.pid)
            return
        }
        var control = identity
        if down {
            control.releaseFlags = event.flags.rawValue
            if event.type == .flagsChanged, let flag = modifierFlag(control.code) {
                control.releaseFlags = event.flags.subtracting(flag).rawValue
            }
            if control.kind == "key", event.type != .flagsChanged {
                let native = NSEvent(cgEvent: event)
                control.releaseText = native?.characters ?? ""
                control.releaseTextIgnoringModifiers = native?.charactersIgnoringModifiers ?? ""
            } else if control.kind == "mouse" {
                try recordMousePosition(event, control: &control)
                control.eventNumber = event.getIntegerValueField(.mouseEventNumber)
                control.clickCount = event.getIntegerValueField(.mouseEventClickState)
            }
        }
        try track(
            control, down: down, request: request, target: target,
            deliver: { deliver(event, target.pid) })
    }

    private static func recordMousePosition(_ event: CGEvent, control: inout Held) throws {
        guard let local = EventWindowLocation.get(event),
            event.location.x.isFinite, event.location.y.isFinite,
            local.x.isFinite, local.y.isFinite
        else {
            throw fail("mouse input has no independent window position")
        }
        control.releaseX = event.location.x
        control.releaseY = event.location.y
        control.releaseLocalX = local.x
        control.releaseLocalY = local.y
    }

    static func modifierFlag(_ code: UInt16) -> CGEventFlags? {
        switch code {
        case 54, 55: return .maskCommand
        case 56, 60: return .maskShift
        case 58, 61: return .maskAlternate
        case 59, 62: return .maskControl
        case 63: return .maskSecondaryFn
        default: return nil
        }
    }

    /// The record precedes a down. A matching up precedes removal of its record.
    /// A crash between either pair leaves only a conservative release to retry.
    static func track(
        _ control: Held, down: Bool, request: HelperRequest,
        target: TargetedInput.Target? = nil, deliver: () -> Void
    )
        throws
    {
        if down {
            try checkCancellation(request)
            var journal = try load(request)
            if let target {
                guard journal.target == nil || journal.target == target else {
                    throw fail("input target changed during this invocation")
                }
                journal.target = target
            }
            guard !journal.held.contains(where: { $0.matches(control) }), journal.held.count < 16
            else {
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
            journal.held.removeAll { $0.matches(control) }
            try save(journal, request: request)
        }
    }

    static func releaseRecorded(_ request: HelperRequest) throws -> Result {
        guard AXIsProcessTrusted() else { throw fail("Accessibility permission is unavailable") }
        let journal = try load(request)
        guard let target = journal.target, target.pid > 0,
            target.launchedAt.isFinite, target.launchedAt > 0, target.windowId > 0,
            !target.bundleId.isEmpty, !Control.isBlocked(target.bundleId),
            let source = CGEventSource(stateID: .privateState)
        else { throw fail("recorded input has no valid independent destination") }
        return try recover(
            journal: journal, request: request,
            // This source never posts to the hardware/session stream. A user's
            // physical hold does not own this process-targeted synthetic down.
            physicallyHeld: { _ in false },
            release: { control in
                let current = try TargetedInput.observe(target).target
                guard sameDestination(current, target) else {
                    throw fail("the original input process or window no longer exists")
                }
                let event = try releaseEvent(control, target: target, source: source)
                event.postToPid(target.pid)
            })
    }

    static func sameDestination(_ current: TargetedInput.Target, _ recorded: TargetedInput.Target)
        -> Bool
    {
        current.pid == recorded.pid && current.bundleId == recorded.bundleId
            && current.launchedAt == recorded.launchedAt && current.windowId == recorded.windowId
    }

    static func releaseEvent(
        _ control: Held, target: TargetedInput.Target,
        source: CGEventSource
    ) throws -> CGEvent {
        let event: CGEvent?
        if control.kind == "mouse" {
            guard let x = control.releaseX, let y = control.releaseY,
                let localX = control.releaseLocalX, let localY = control.releaseLocalY,
                x.isFinite, y.isFinite, localX.isFinite, localY.isFinite
            else {
                throw fail("recorded mouse input has no independent position")
            }
            event =
                NSEvent.mouseEvent(
                    with: control.code == 0 ? .leftMouseUp : .rightMouseUp,
                    location: .zero, modifierFlags: [],
                    timestamp: ProcessInfo.processInfo.systemUptime,
                    windowNumber: Int(target.windowId), context: nil,
                    eventNumber: Int(control.eventNumber ?? 0),
                    clickCount: Int(control.clickCount ?? 1),
                    pressure: 0)?.cgEvent
            event?.location = CGPoint(x: x, y: y)
            if let event {
                try EventWindowLocation.set(event, point: CGPoint(x: localX, y: localY))
            }
            event?.setIntegerValueField(
                .mouseEventWindowUnderMousePointer, value: Int64(target.windowId))
            event?.setIntegerValueField(
                .mouseEventWindowUnderMousePointerThatCanHandleThisEvent,
                value: Int64(target.windowId))
        } else {
            event =
                NSEvent.keyEvent(
                    with: .keyUp, location: .zero, modifierFlags: [],
                    timestamp: ProcessInfo.processInfo.systemUptime,
                    windowNumber: Int(target.windowId),
                    context: nil, characters: control.releaseText ?? "",
                    charactersIgnoringModifiers: control.releaseTextIgnoringModifiers ?? control
                        .releaseText
                        ?? "",
                    isARepeat: false,
                    keyCode: control.code)?.cgEvent
            if modifierFlag(control.code) != nil { event?.type = .flagsChanged }
        }
        guard let event else { throw fail("could not construct independent release") }
        event.setSource(source)
        event.setIntegerValueField(.eventTargetUnixProcessID, value: Int64(target.pid))
        event.flags = CGEventFlags(rawValue: control.releaseFlags ?? 0)
        if control.kind == "key", modifierFlag(control.code) == nil,
            let text = control.releaseText, !text.isEmpty
        {
            let units = Array(text.utf16)
            event.keyboardSetUnicodeString(stringLength: units.count, unicodeString: units)
        }
        event.timestamp = DispatchTime.now().uptimeNanoseconds
        return event
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
