import AppKit
import CoreGraphics
import Foundation

/// Sends private-source input to one observed process and window. The caller
/// verifies app effects; successful delivery does not prove the action worked.
enum TargetedInput {
    struct Target: Codable, Equatable {
        let pid: pid_t
        let bundleId: String
        let launchedAt: TimeInterval
        let windowId: CGWindowID
        let frame: CGRect
    }

    struct Observation: Equatable {
        let target: Target
        let frontmostPid: pid_t?
        let pointer: CGPoint
    }

    struct Step {
        let event: CGEvent
        let release: CGEvent?
        let delay: TimeInterval
    }

    struct Failure: Error, CustomStringConvertible {
        let reason: String
        let delivered: Int
        let released: Int
        var description: String {
            "\(reason); delivered=\(delivered), targeted releases=\(released)"
        }
    }

    static func observe(_ target: Target) throws -> Observation {
        guard let app = NSRunningApplication(processIdentifier: target.pid),
            !app.isTerminated, app.bundleIdentifier == target.bundleId,
            let launchedAt = app.launchDate?.timeIntervalSince1970,
            let windows = CGWindowListCopyWindowInfo(.optionIncludingWindow, target.windowId)
                as? [[String: Any]],
            let window = windows.first(where: {
                ($0[kCGWindowNumber as String] as? NSNumber)?.uint32Value == target.windowId
                    && ($0[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == target.pid
            }),
            let bounds = window[kCGWindowBounds as String] as? [String: Any],
            let frame = CGRect(dictionaryRepresentation: bounds as CFDictionary),
            let pointer = CGEvent(source: nil)?.location
        else { throw Failure(reason: "target identity is unavailable", delivered: 0, released: 0) }
        return Observation(
            target: Target(
                pid: target.pid, bundleId: target.bundleId, launchedAt: launchedAt,
                windowId: target.windowId, frame: frame),
            frontmostPid: NSWorkspace.shared.frontmostApplication?.processIdentifier,
            pointer: pointer)
    }

    final class Session {
        let target: Target
        let source: CGEventSource
        private let baseline: Observation
        private let observation: () throws -> Observation
        private let checkCancellation: () throws -> Void
        private let deliver: (CGEvent, pid_t) -> Void
        private let pause: (TimeInterval) -> Void
        private let requireUnchangedDesktop: Bool

        init(
            target: Target, observation: @escaping () throws -> Observation,
            checkCancellation: @escaping () throws -> Void,
            deliver: @escaping (CGEvent, pid_t) -> Void,
            pause: @escaping (TimeInterval) -> Void,
            requireUnchangedDesktop: Bool = true
        ) throws {
            guard target.pid > 0, !target.bundleId.isEmpty,
                target.launchedAt.isFinite, target.launchedAt > 0,
                target.windowId > 0, target.frame.width > 0, target.frame.height > 0,
                let source = CGEventSource(stateID: .privateState)
            else { throw Failure(reason: "invalid input target", delivered: 0, released: 0) }
            let baseline = try observation()
            guard baseline.target == target, baseline.frontmostPid != target.pid else {
                throw Failure(
                    reason: "target must match and remain inactive", delivered: 0, released: 0)
            }
            self.target = target
            self.source = source
            self.baseline = baseline
            self.observation = observation
            self.checkCancellation = checkCancellation
            self.deliver = deliver
            self.pause = pause
            self.requireUnchangedDesktop = requireUnchangedDesktop
        }

        func validate(releasing: Bool = false) throws {
            let current = try observation()
            let sameProcess =
                current.target.pid == target.pid
                && current.target.bundleId == target.bundleId
                && current.target.launchedAt == target.launchedAt
                && current.target.windowId == target.windowId
            guard sameProcess else {
                throw Failure(reason: "input target changed", delivered: 0, released: 0)
            }
            if !releasing {
                try checkCancellation()
                guard current.target == target, current.frontmostPid != target.pid,
                    !requireUnchangedDesktop || current == baseline
                else {
                    throw Failure(
                        reason: "input target changed or is in use", delivered: 0, released: 0)
                }
            }
        }

        /// Production sends through the broker-owned journal. Recovery preserves
        /// this same destination when cancellation or transport interrupts input.
        func post(_ event: CGEvent, request: HelperRequest) throws {
            let releasing = InputRecovery.heldControl(event).map { !$0.1 } ?? false
            try validate(releasing: releasing)
            event.timestamp = DispatchTime.now().uptimeNanoseconds
            try InputRecovery.post(event, request: request, target: target, deliver: deliver)
        }

        /// Public Quartz fields bind pointer routing to the observed window.
        /// No raw event-field numbers or global event-tap fallback are used.
        private func bind(_ event: CGEvent, mouse: Bool) -> CGEvent {
            event.setSource(source)
            event.setIntegerValueField(.eventTargetUnixProcessID, value: Int64(target.pid))
            if mouse {
                event.setIntegerValueField(
                    .mouseEventWindowUnderMousePointer,
                    value: Int64(target.windowId))
                event.setIntegerValueField(
                    .mouseEventWindowUnderMousePointerThatCanHandleThisEvent,
                    value: Int64(target.windowId))
            }
            return event
        }

        func mouse(_ type: CGEventType, at point: CGPoint, clickCount: Int64 = 1) throws -> CGEvent
        {
            let nativeType: NSEvent.EventType
            switch type {
            case .leftMouseDown: nativeType = .leftMouseDown
            case .leftMouseUp: nativeType = .leftMouseUp
            case .leftMouseDragged: nativeType = .leftMouseDragged
            case .rightMouseDown: nativeType = .rightMouseDown
            case .rightMouseUp: nativeType = .rightMouseUp
            case .rightMouseDragged: nativeType = .rightMouseDragged
            case .mouseMoved: nativeType = .mouseMoved
            default:
                throw Failure(
                    reason: "unsupported independent mouse event", delivered: 0, released: 0)
            }
            guard point.x.isFinite, point.y.isFinite, target.frame.contains(point),
                let event = NSEvent.mouseEvent(
                    with: nativeType,
                    location: CGPoint(
                        x: point.x - target.frame.minX, y: target.frame.maxY - point.y),
                    modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                    windowNumber: Int(target.windowId), context: nil, eventNumber: 1,
                    clickCount: Int(clickCount),
                    pressure: type == .leftMouseUp || type == .rightMouseUp ? 0 : 1)?.cgEvent
            else {
                throw Failure(reason: "pointer target leaves the window", delivered: 0, released: 0)
            }
            // The receiver resolves its own AppKit window from windowNumber.
            // Quartz carries the observed global point across the process boundary.
            event.location = point
            // CoreGraphics stores this SPI field from the window's top edge;
            // AppKit converts it to bottom-origin locationInWindow on receipt.
            try EventWindowLocation.set(
                event,
                point: CGPoint(
                    x: point.x - target.frame.minX, y: point.y - target.frame.minY))
            return bind(event, mouse: true)
        }

        func scroll(at point: CGPoint, dx: Double, dy: Double) throws -> CGEvent {
            guard dx.isFinite, dy.isFinite,
                let reference = CGEvent(
                    scrollWheelEvent2Source: source, units: .pixel, wheelCount: 2,
                    wheel1: Control.scrollWheelDelta(dy), wheel2: Control.scrollWheelDelta(dx),
                    wheel3: 0)
            else {
                throw Failure(reason: "cannot construct targeted scroll", delivered: 0, released: 0)
            }
            let event = try mouse(.mouseMoved, at: point)
            event.type = .scrollWheel
            for field: CGEventField in [
                .scrollWheelEventDeltaAxis1, .scrollWheelEventDeltaAxis2,
                .scrollWheelEventDeltaAxis3, .scrollWheelEventFixedPtDeltaAxis1,
                .scrollWheelEventFixedPtDeltaAxis2, .scrollWheelEventFixedPtDeltaAxis3,
                .scrollWheelEventPointDeltaAxis1, .scrollWheelEventPointDeltaAxis2,
                .scrollWheelEventPointDeltaAxis3, .scrollWheelEventScrollPhase,
                .scrollWheelEventScrollCount, .scrollWheelEventMomentumPhase,
                .scrollWheelEventIsContinuous,
            ] {
                event.setIntegerValueField(field, value: reference.getIntegerValueField(field))
            }
            return event
        }

        func key(
            _ code: CGKeyCode, down: Bool, characters: String? = nil,
            flags: CGEventFlags = []
        ) throws -> CGEvent {
            guard code <= 127,
                let translated = CGEvent(
                    keyboardEventSource: source, virtualKey: code, keyDown: down)
            else {
                throw Failure(reason: "cannot translate targeted key", delivered: 0, released: 0)
            }
            // Translate with the final flags before binding the AppKit window.
            // Adding flags after an explicit character string keeps it unshifted.
            translated.flags = flags
            guard let native = NSEvent(cgEvent: translated) else {
                throw Failure(reason: "cannot translate targeted key", delivered: 0, released: 0)
            }
            let text = characters ?? native.characters ?? ""
            let unmodifiedText = characters ?? native.charactersIgnoringModifiers ?? ""
            guard text.utf16.count <= 8, unmodifiedText.utf16.count <= 8,
                let event = NSEvent.keyEvent(
                    with: down ? .keyDown : .keyUp,
                    location: .zero, modifierFlags: native.modifierFlags,
                    timestamp: ProcessInfo.processInfo.systemUptime,
                    windowNumber: Int(target.windowId), context: nil, characters: text,
                    charactersIgnoringModifiers: unmodifiedText, isARepeat: false, keyCode: code)?
                    .cgEvent
            else {
                throw Failure(reason: "cannot construct targeted key", delivered: 0, released: 0)
            }
            event.flags = flags
            if let characters, !characters.isEmpty {
                let units = Array(characters.utf16)
                event.keyboardSetUnicodeString(stringLength: units.count, unicodeString: units)
            }
            return bind(event, mouse: false)
        }

        func textSteps(_ text: String, maxUnits: Int = 500) throws -> [Step] {
            guard !text.isEmpty, text.utf16.count <= maxUnits else {
                throw HelperError(
                    code: .invalidRequest,
                    message: "independent text entry requires 1 to \(maxUnits) UTF-16 units")
            }
            return try text.unicodeScalars.flatMap { scalar -> [Step] in
                let characters = String(scalar)
                let down = try key(0, down: true, characters: characters)
                let up = try key(0, down: false, characters: characters)
                return [
                    .init(event: down, release: up, delay: 0.004),
                    .init(event: up, release: nil, delay: 0.004),
                ]
            }
        }

        /// Production uses the broker journal for every held input and movement.
        /// A failure after dispatch reports an uncertain outcome, even if release succeeds.
        func perform(_ steps: [Step], request: HelperRequest) throws {
            guard !steps.isEmpty, steps.count <= 1002,
                steps.reduce(0, { $0 + $1.delay }) <= 10.001,
                steps.allSatisfy({ $0.delay.isFinite && $0.delay >= 0 && $0.delay <= 0.1 })
            else {
                throw HelperError(
                    code: .invalidRequest, message: "invalid independent input sequence")
            }
            var sent = false
            var pending: CGEvent?
            func release() throws {
                guard let event = pending else { return }
                try post(event, request: request)
                pending = nil
            }
            do {
                for step in steps {
                    try validate()
                    if step.release != nil, pending != nil {
                        throw Failure(reason: "an input is already held", delivered: 0, released: 0)
                    }
                    try post(step.event, request: request)
                    sent = true
                    if let up = step.release { pending = up }
                    if let held = InputRecovery.heldControl(step.event), !held.1 {
                        pending = nil
                    }
                    if let up = pending, up.type == .leftMouseUp || up.type == .rightMouseUp {
                        up.location = step.event.location
                        try EventWindowLocation.set(
                            up,
                            point: CGPoint(
                                x: up.location.x - target.frame.minX,
                                y: up.location.y - target.frame.minY))
                    }
                    pause(step.delay)
                }
                try release()
                try validate()
            } catch {
                do { try release() } catch {
                    throw HelperError(
                        code: .operationFailed,
                        message: "could not confirm independent input release: \(error)")
                }
                if sent {
                    throw HelperError(
                        code: .operationFailed,
                        message: "independent input stopped after dispatch: \(error)")
                }
                throw error
            }
        }

        /// Pair every accepted down with a release to the same process. A
        /// changed foreground or pointer stops new input without restoring it.
        /// Process replacement blocks even release: a reused PID is a new app.
        func run(_ steps: [Step]) throws -> (delivered: Int, released: Int) {
            var delivered = 0
            var released = 0
            var pending: CGEvent?
            func release() throws {
                guard let event = pending else { return }
                let current = try observation().target
                guard current.pid == target.pid, current.bundleId == target.bundleId,
                    current.launchedAt == target.launchedAt, current.windowId == target.windowId
                else {
                    throw Failure(
                        reason: "target changed before release", delivered: delivered,
                        released: released)
                }
                event.timestamp = DispatchTime.now().uptimeNanoseconds
                deliver(event, target.pid)
                released += 1
                pending = nil
            }
            do {
                guard !steps.isEmpty, steps.count <= 102,
                    steps.reduce(0, { $0 + $1.delay }) <= 1,
                    steps.allSatisfy({ $0.delay.isFinite && $0.delay >= 0 && $0.delay <= 0.05 })
                else { throw Failure(reason: "invalid event plan", delivered: 0, released: 0) }
                for step in steps {
                    try validate()
                    if let up = step.release {
                        guard pending == nil else {
                            throw Failure(
                                reason: "an input is already held", delivered: delivered,
                                released: released)
                        }
                        pending = up
                    }
                    step.event.timestamp = DispatchTime.now().uptimeNanoseconds
                    deliver(step.event, target.pid)
                    delivered += 1
                    if let held = InputRecovery.heldControl(step.event), !held.1 {
                        pending = nil
                    }
                    if let up = pending, up.type == .leftMouseUp || up.type == .rightMouseUp {
                        up.location = step.event.location
                        try EventWindowLocation.set(
                            up,
                            point: CGPoint(
                                x: up.location.x - target.frame.minX,
                                y: up.location.y - target.frame.minY))
                    }
                    pause(step.delay)
                }
                try release()
                try validate()
                return (delivered, released)
            } catch {
                do { try release() } catch {
                    throw Failure(
                        reason: "cannot confirm targeted release: \(error)", delivered: delivered,
                        released: released)
                }
                throw Failure(
                    reason: String(describing: error), delivered: delivered, released: released)
            }
        }
    }
}
