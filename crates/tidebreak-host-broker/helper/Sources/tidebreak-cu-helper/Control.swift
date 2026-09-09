import AppKit
import ApplicationServices
import CoreGraphics
import Foundation

/// Input synthesis — the "acting" half of computer use. The agent reads an
/// app's accessibility tree (`AXTree`), then drives it: click an element, type
/// into a field, press a key chord, scroll, focus a window. AX-first (act on
/// the element via `AXUIElementPerformAction` / `AXUIElementSetAttributeValue`),
/// falling back to `CGEvent` coordinate synthesis only when the element exposes
/// no usable action.
///
/// The broker owns policy (the per-app control grant and the act-time
/// consequential gate). This helper is a dumb executor — but it carries a
/// defensive copy of the never-automate blocklist so a buggy or compromised
/// broker cannot drive a terminal, Tidebreak itself, or the system auth/login
/// surfaces.
enum Control {
    /// Returned to the broker for every control op. `usedFallback` is true when
    /// AX targeting was not available and a coordinate/keystroke synthesis was
    /// used instead — useful signal for the agent.
    struct Result: Encodable {
        let executionMode: ExecutionMode
        let success: Bool
        let usedFallback: Bool
        let detail: String?
    }

    /// Returned for `describe_element`: the target element's normalized role +
    /// label, read without acting, for the broker's forced-confirmation
    /// tripwire, plus the element's current fingerprint so the broker can bind
    /// the confirmation to the exact element it showed (a swapped element with
    /// the same label has a different fingerprint). Any field may be null (no
    /// addressed element, or it no longer resolves — the broker treats nulls as
    /// benign / fails open).
    struct DescribeResult: Encodable {
        let role: String?
        let label: String?
        let fingerprint: String?
    }

    /// Returned for `wait_condition`.
    struct WaitResult: Encodable {
        let met: Bool
        let timedOut: Bool
    }

    /// Internal app/window record for resize/visibility checks. Mirrors the
    /// wire `Window` shape but stays private to this file.
    struct AppWindow {
        let windowId: UInt32
        let ownerPid: pid_t
        let frame: CGRect
    }

    // MARK: - Never-automate blocklist (defensive copy; the broker is authoritative)

    /// Bundle ids (exact, or as a dotted prefix) the helper will never act on.
    /// Mirrors the broker's blocklist — defense in depth, not the primary gate.
    static let blockedBundlePrefixes: [String] = [
        "io.brightwave.tidebreak",
        "com.apple.loginwindow",
        "com.apple.SecurityAgent",
        "com.apple.CoreAuthUI",
        "com.apple.coreauthd",
        "com.apple.systempreferences",
        "com.apple.keychainaccess",
    ]

    /// Matches the broker's semantics exactly: an entry blocks its exact id and
    /// anything nested under it at a dotted boundary, so `com.apple.SecurityAgent`
    /// blocks `com.apple.SecurityAgent.helper` but not a lookalike suffix.
    static func isBlocked(_ bundleId: String) -> Bool {
        blockedBundlePrefixes.contains { entry in
            let base = entry.hasSuffix(".") ? String(entry.dropLast()) : entry
            return bundleId == base
                || (bundleId.hasPrefix(base) && bundleId.dropFirst(base.count).hasPrefix("."))
        }
    }

    /// Throw `operation_failed` when a request targets a blocked bundle. Shared
    /// by every op that names an app — capture and read as well as control, so
    /// a compromised broker cannot read protected security or Tidebreak surfaces
    /// directly.
    static func ensureNotBlocked(_ bundleId: String?) throws {
        if let bundleId, isBlocked(bundleId) {
            throw HelperError(
                code: .operationFailed, message: "app \(bundleId) is not automatable")
        }
    }

    // MARK: - Auto-yield on system security dialogs

    /// Bundle ids whose presence as the frontmost app means a system
    /// security / authorization surface owns the screen — TCC / admin-password
    /// / "wants to control your computer" prompts, the login/unlock screen,
    /// Touch ID / local-auth panels, app-switcher overlays, and other modal
    /// system dialogs. These are focus-stealing, and exactly where the user is
    /// mid-authentication or mid-choice, so synthesized input could drive a
    /// click or keystroke straight into a surface the agent must never touch.
    /// Distinct from `blockedBundlePrefixes` (which refuses a target app):
    /// this refuses to act at all while one of these owns the foreground,
    /// whatever the target.
    private static let systemDialogFrontmostPrefixes: [String] = [
        "com.apple.SecurityAgent",
        "com.apple.loginwindow",
        "com.apple.CoreAuthUI",
        "com.apple.coreauthd",
        // The macOS "password" / "wants to control your computer" prompts and
        // app-switcher overlays are fronted by Morpha and Pashua bundles.
        "org.morpha.dialog",
        "org.pashua.Pashua",
        // Core dialog hosts seen on current macOS.
        "com.apple.MTile",
        "com.apple.NetworkBrowserAgent",
    ]

    /// Throw `.yielded` if a system security/authorization dialog currently
    /// owns the foreground, so the agent backs off instead of driving input
    /// into it. Cheap (one frontmost-app read); called at the start of every
    /// acting op (never the read-only `describe_element`, which must stay
    /// available for the tripwire).
    private static func ensureNoSystemDialogFrontmost() throws {
        guard let frontmost = NSWorkspace.shared.frontmostApplication?.bundleIdentifier else {
            return
        }
        if systemDialogFrontmostPrefixes.contains(where: {
            frontmost == $0 || frontmost.hasPrefix($0)
        }) {
            throw HelperError(
                code: .yielded,
                message:
                    "a system security dialog (\(frontmost)) is in the foreground; stopped instead of synthesizing input into it"
            )
        }
    }

    /// The broker rotates this generation when control stops or resumes.
    static func ensureNotCancelled(_ request: HelperRequest) throws {
        try InputRecovery.checkCancellation(request)
        guard request.cancelPath != nil || request.cancelGeneration != nil else { return }
        guard let path = request.cancelPath, let generation = request.cancelGeneration,
            !generation.isEmpty, generation != "stopped",
            let data = FileManager.default.contents(atPath: path), data.count <= 1024,
            let actual = String(data: data, encoding: .utf8),
            actual.trimmingCharacters(in: .whitespacesAndNewlines) == generation
        else {
            throw HelperError(
                code: .yielded, message: "computer use stopped or its control session changed")
        }
    }

    // MARK: - Ops

    static func click(_ request: HelperRequest) throws -> Result {
        try ensureNotCancelled(request)
        let app = try requireControllableApp(request)
        try ensureNoSystemDialogFrontmost()
        try prepareForInput(app, request: request)
        let button = request.button ?? "left"
        let count = request.clickCount ?? 1

        if request.executionMode != .foreground {
            guard button == "left", count == 1 else {
                throw HelperError(
                    code: .requiresForeground,
                    message: "background clicks support a single Accessibility press")
            }
            let element: AXUIElement
            if request.elementId != nil {
                element = try resolveElement(app: app, request: request)
            } else {
                element = try resolveBackgroundPoint(app: app, request: request)
            }
            let role = AXTree.copyString(element, kAXRoleAttribute as CFString)
            guard role != kAXPopUpButtonRole, role != kAXMenuItemRole else {
                throw HelperError(
                    code: .requiresForeground,
                    message: "native menu interaction requires foreground computer use")
            }
            try ensureNotCancelled(request)
            guard AXUIElementPerformAction(element, kAXPressAction as CFString) == .success else {
                throw HelperError(
                    code: .requiresForeground,
                    message: "the target has no supported background press action")
            }
            return Result(
                executionMode: .background, success: true, usedFallback: false, detail: "AXPress")
        }

        if request.elementId != nil {
            let element = try resolveElement(app: app, request: request)
            // AXPress is the clean path for a plain single left click — no
            // coordinates, no Retina math.
            if button == "left", count <= 1 {
                try ensureNotCancelled(request)
                if AXUIElementPerformAction(element, kAXPressAction as CFString) == .success {
                    return Result(
                        executionMode: request.executionMode ?? .background, success: true,
                        usedFallback: false, detail: "AXPress")
                }
            }
            // Right / double / AXPress-unsupported → synthesize at the element's
            // on-screen center.
            guard let center = elementCenter(element) else {
                throw HelperError(
                    code: .operationFailed, message: "element has no on-screen frame to click")
            }
            try ensurePointInApp(center, app: app, request: request)
            try postMouseClick(
                at: center, button: button, clickCount: count, app: app, request: request)
            return Result(
                executionMode: request.executionMode ?? .background, success: true,
                usedFallback: true, detail: "synthesized \(button) click")
        }

        guard let point = explicitPoint(request) else {
            throw HelperError(code: .invalidRequest, message: "click requires element_id or x/y")
        }
        // A raw point is global; confine it to the granted app's windows so a
        // click cannot land on another app's (or Tidebreak's own) surface.
        try ensurePointInApp(point, app: app, request: request)
        try postMouseClick(at: point, button: button, clickCount: count, app: app, request: request)
        return Result(
            executionMode: request.executionMode ?? .background, success: true, usedFallback: true,
            detail: "synthesized click at point")
    }

    static func typeText(_ request: HelperRequest) throws -> Result {
        try ensureNotCancelled(request)
        let app = try requireControllableApp(request)
        try ensureNoSystemDialogFrontmost()
        guard let text = request.text else {
            throw HelperError(code: .invalidRequest, message: "type_text requires text")
        }

        if request.executionMode != .foreground {
            guard request.elementId != nil else {
                throw HelperError(
                    code: .requiresForeground,
                    message: "background typing requires an Accessibility text element")
            }
            let element = try resolveElement(app: app, request: request)
            try ensureNotCancelled(request)
            guard
                AXUIElementSetAttributeValue(
                    element, kAXValueAttribute as CFString, text as CFString) == .success
            else {
                throw HelperError(
                    code: .requiresForeground,
                    message: "the text element does not support a background value update")
            }
            guard let value = AXTree.copyAttr(element, kAXValueAttribute as CFString),
                CFGetTypeID(value) == CFStringGetTypeID(), value as! String == text
            else {
                throw HelperError(
                    code: .operationFailed,
                    message: "the text element did not retain the requested value")
            }
            return Result(
                executionMode: .background, success: true, usedFallback: false,
                detail: "verified AXValue")
        }

        if request.elementId != nil {
            let element = try resolveElement(app: app, request: request)
            try ensureNotCancelled(request)
            AXUIElementSetAttributeValue(
                element, kAXFocusedAttribute as CFString, true as CFTypeRef)
            // Setting AXValue is the clean path for a text field; if the element
            // rejects it (not a value-bearing role), fall back to synthesizing
            // keystrokes into the now-focused element.
            try ensureNotCancelled(request)
            if AXUIElementSetAttributeValue(
                element, kAXValueAttribute as CFString, text as CFString)
                == .success
            {
                return Result(
                    executionMode: request.executionMode ?? .background, success: true,
                    usedFallback: false, detail: "set AXValue")
            }
            // Synthesized keystrokes go to the frontmost app, not the element's
            // PID, and setting AXFocused does not reliably foreground the app —
            // bring it forward first (as keyPress and the no-element path below
            // do) so the text cannot land in a different window.
            try prepareForInput(app, request: request)
            try typeUnicode(text, app: app, request: request)
            return Result(
                executionMode: request.executionMode ?? .background, success: true,
                usedFallback: true, detail: "synthesized keystrokes")
        }

        try prepareForInput(app, request: request)
        try typeUnicode(text, app: app, request: request)
        return Result(
            executionMode: request.executionMode ?? .background, success: true, usedFallback: true,
            detail: "synthesized keystrokes")
    }

    static func keyPress(_ request: HelperRequest) throws -> Result {
        try requireForeground(request, operation: "key_press")
        try ensureNotCancelled(request)
        let app = try requireControllableApp(request)
        try ensureNoSystemDialogFrontmost()
        guard let keyName = request.key else {
            throw HelperError(code: .invalidRequest, message: "key_press requires key")
        }
        guard let keyCode = virtualKeyCode(for: keyName) else {
            throw HelperError(code: .invalidRequest, message: "unknown key: \(keyName)")
        }
        // A chord targets the focused app, so bring it forward first.
        try prepareForInput(app, request: request)
        // A real event source (not nil) delivers far more reliably than a fresh
        // nil source. With a nil source, apps that read modifier/key state from
        // the session (Electron, browsers) intermittently drop synthesized
        // chords and the bare Return that submits a field.
        let source = eventSource(request)
        let modifiers = resolveModifiers(request.modifiers ?? [])
        guard
            let down = CGEvent(keyboardEventSource: source, virtualKey: keyCode, keyDown: true),
            let up = CGEvent(keyboardEventSource: source, virtualKey: keyCode, keyDown: false)
        else {
            throw HelperError(code: .operationFailed, message: "could not synthesize key event")
        }

        // Press the real modifier keys before the main key, not just the event
        // flag. Setting `.flags` alone is enough for AppKit apps, but
        // Electron/Chromium track modifier state from actual key-down events; a
        // flag riding on a lone character event is ignored there, so the chord
        // collapses to the bare character. Posting the command key down first
        // makes those apps see a genuine shortcut.
        //
        // Synthesize every modifier event up front, before posting any of them.
        // `CGEvent` creation can fail, and this helper posts to the system-wide
        // HID tap then exits per operation: a failure after a modifier-down was
        // already posted would leave that modifier latched for the user's real
        // keyboard with no chance to release it. Building everything first
        // means a synthesis failure aborts before any state is mutated.
        var modifierDowns: [CGEvent] = []
        var modifierUps: [CGEvent] = []
        var flags: CGEventFlags = []
        for modifier in modifiers {
            flags.insert(modifier.flag)
            guard
                let modDown = CGEvent(
                    keyboardEventSource: source, virtualKey: modifier.keyCode, keyDown: true)
            else {
                throw HelperError(
                    code: .operationFailed, message: "could not synthesize modifier event")
            }
            modDown.flags = flags
            modifierDowns.append(modDown)
        }
        let chordMask = flags
        // Release events, in reverse, clearing each flag as we go so no modifier
        // is left logically stuck down for the user's subsequent real input.
        for modifier in modifiers.reversed() {
            flags.remove(modifier.flag)
            guard
                let modUp = CGEvent(
                    keyboardEventSource: source, virtualKey: modifier.keyCode, keyDown: false)
            else {
                throw HelperError(
                    code: .operationFailed, message: "could not synthesize modifier event")
            }
            modUp.flags = flags
            modifierUps.append(modUp)
        }
        down.flags = chordMask
        up.flags = chordMask

        var pressedModifiers = 0
        var keyIsDown = false
        defer {
            if keyIsDown { try? post(up, app: app, request: request) }
            for modUp in modifierUps.suffix(pressedModifiers) {
                try? post(modUp, app: app, request: request)
            }
        }
        for modDown in modifierDowns {
            try ensureNotCancelled(request)
            try post(modDown, app: app, request: request)
            pressedModifiers += 1
        }
        if !modifiers.isEmpty { usleep(keyPressHoldMicros) }
        try ensureNotCancelled(request)
        try post(down, app: app, request: request)
        keyIsDown = true
        usleep(keyPressHoldMicros)
        try ensureNotCancelled(request)
        try post(up, app: app, request: request)
        keyIsDown = false
        return Result(
            executionMode: request.executionMode ?? .background, success: true, usedFallback: false,
            detail: "key \(keyName)")
    }

    static func scroll(_ request: HelperRequest) throws -> Result {
        try ensureNotCancelled(request)
        let app = try requireControllableApp(request)
        try ensureNoSystemDialogFrontmost()
        let dx = request.dx ?? 0
        let dy = request.dy ?? 0

        if request.executionMode != .foreground {
            return try scrollAccessibility(app: app, request: request)
        }
        try prepareForInput(app, request: request)
        let point: CGPoint
        if request.elementId != nil || explicitPoint(request) != nil {
            point = try resolveTargetPoint(app: app, request: request)
        } else if request.executionMode == .foreground, let cursor = CGEvent(source: nil)?.location
        {
            point = cursor
        } else {
            throw HelperError(
                code: .operationFailed, message: "background scrolling requires element_id or x/y")
        }
        try ensurePointInApp(point, app: app, request: request)
        guard dx.isFinite, dy.isFinite else {
            throw HelperError(code: .invalidRequest, message: "scroll deltas must be finite")
        }

        // The deltas feed a CGEvent's Int32 wheel fields; a finite but huge
        // delta (a model asking to "scroll to the bottom" with dy=1e10) would
        // trap the Int32 conversion and crash the process. Clamp instead of
        // crashing — an enormous scroll is an enormous scroll.
        let clampedDy = scrollWheelDelta(dy)
        let clampedDx = scrollWheelDelta(dx)
        guard
            let event = CGEvent(
                scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 2,
                wheel1: clampedDy, wheel2: clampedDx, wheel3: 0)
        else {
            throw HelperError(code: .operationFailed, message: "could not synthesize scroll event")
        }
        guard
            let move = CGEvent(
                mouseEventSource: eventSource(request),
                mouseType: .mouseMoved, mouseCursorPosition: point, mouseButton: .left)
        else {
            throw HelperError(
                code: .operationFailed, message: "could not position the pointer for scrolling")
        }
        try ensureNotCancelled(request)
        try post(move, app: app, request: request)
        usleep(20_000)
        event.location = point
        try ensureNotCancelled(request)
        try post(event, app: app, request: request)
        return Result(
            executionMode: request.executionMode ?? .background, success: true, usedFallback: true,
            detail: "scroll dx \(dx) dy \(dy)")
    }

    /// Positive API deltas move content down/right; CG wheel deltas use the opposite sign.
    static func scrollWheelDelta(_ delta: Double) -> Int32 {
        Int32(min(max(-delta.rounded(), Double(Int32.min)), Double(Int32.max)))
    }

    static func focusWindow(_ request: HelperRequest) throws -> Result {
        try requireForeground(request, operation: "focus_window")
        try ensureNotCancelled(request)
        let app = try requireControllableApp(request)
        try ensureNoSystemDialogFrontmost()
        // This is the agent's path back to an app after focus shifted away. A
        // bare async activate() loses the race against a just-activated window,
        // so wait for the app to actually become frontmost before reporting
        // success.
        try prepareForInput(app, request: request)
        guard let windowId = request.windowId else {
            return Result(
                executionMode: request.executionMode ?? .background, success: true,
                usedFallback: false, detail: "focused app")
        }
        guard
            windows(pid: app.processIdentifier, bundleId: app.bundleIdentifier)
                .contains(where: { $0.windowId == windowId })
        else {
            throw HelperError(
                code: .staleElement, message: "the requested window no longer belongs to the app")
        }
        let appElement = AXTree.appElement(for: app.processIdentifier)
        guard let window = accessibleWindowElement(matching: windowId, appElement: appElement)
        else {
            throw HelperError(
                code: .operationFailed, message: "the requested window has no accessibility target")
        }
        try ensureNotCancelled(request)
        guard AXUIElementPerformAction(window, kAXRaiseAction as CFString) == .success else {
            throw HelperError(
                code: .operationFailed, message: "the app rejected the window focus request")
        }
        try ensureNotCancelled(request)
        AXUIElementSetAttributeValue(window, kAXMainAttribute as CFString, kCFBooleanTrue)
        let deadline = ProcessInfo.processInfo.systemUptime + 1
        repeat {
            try ensureNotCancelled(request)
            if let focused = AXTree.copyAttr(appElement, kAXFocusedWindowAttribute as CFString),
                CFGetTypeID(focused) == AXUIElementGetTypeID(), CFEqual(focused, window)
            {
                return Result(
                    executionMode: request.executionMode ?? .background, success: true,
                    usedFallback: false, detail: "verified focused window")
            }
            usleep(20_000)
        } while ProcessInfo.processInfo.systemUptime < deadline
        throw HelperError(
            code: .operationFailed, message: "the requested window did not become focused")
    }

    /// Launch a registered bundle and confirm its process exists before return.
    static func launchApp(_ request: HelperRequest) async throws -> Result {
        try ensureNotCancelled(request)
        guard let bundleId = request.bundleId, !bundleId.isEmpty else {
            throw HelperError(code: .invalidRequest, message: "launch_app requires bundle_id")
        }
        try ensureNotBlocked(bundleId)
        try ensureNoSystemDialogFrontmost()
        if let running = NSWorkspace.shared.runningApplications.first(where: {
            $0.bundleIdentifier == bundleId && !$0.isTerminated
        }) {
            try prepareForInput(running, request: request)
            return Result(
                executionMode: request.executionMode ?? .background,
                success: true, usedFallback: false,
                detail: request.executionMode == .foreground
                    ? "activated running \(bundleId)" : "application is already running")
        }
        guard let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleId) else {
            throw HelperError(
                code: .notFound, message: "registered application \(bundleId) not found")
        }
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.activates = request.executionMode == .foreground
        try ensureNotCancelled(request)
        let app = try await NSWorkspace.shared.openApplication(
            at: url, configuration: configuration)
        guard app.bundleIdentifier == bundleId, !app.isTerminated else {
            throw HelperError(
                code: .operationFailed, message: "the requested application did not start")
        }
        return Result(
            executionMode: request.executionMode ?? .background, success: true, usedFallback: false,
            detail: "launched \(bundleId)")
    }

    /// Move the pointer over an element or confined coordinate point without
    /// pressing.
    static func hover(_ request: HelperRequest) throws -> Result {
        try requireForeground(request, operation: "hover")
        try ensureNotCancelled(request)
        let app = try requireControllableApp(request)
        try ensureNoSystemDialogFrontmost()
        try prepareForInput(app, request: request)
        let point = try resolveTargetPoint(app: app, request: request)
        try ensurePointInApp(point, app: app, request: request)
        guard
            let move = CGEvent(
                mouseEventSource: eventSource(request),
                mouseType: .mouseMoved,
                mouseCursorPosition: point, mouseButton: .left)
        else {
            throw HelperError(
                code: .operationFailed, message: "could not synthesize pointer movement")
        }
        try ensureNotCancelled(request)
        try post(move, app: app, request: request)
        usleep(50_000)
        return Result(
            executionMode: request.executionMode ?? .background, success: true, usedFallback: true,
            detail: "posted pointer movement")
    }

    /// Press, drag through a bounded number of steps, and release. Both
    /// endpoints are resolved and confined before the first mouse-down, so a
    /// mid-drag test cannot leave a different app holding the button.
    static func drag(_ request: HelperRequest) throws -> Result {
        try requireForeground(request, operation: "drag")
        try ensureNotCancelled(request)
        let app = try requireControllableApp(request)
        try ensureNoSystemDialogFrontmost()
        try prepareForInput(app, request: request)
        let from = try resolveTargetPoint(app: app, request: request, prefix: "from")
        let to = try resolveTargetPoint(app: app, request: request, prefix: "to")
        let points = dragPoints(from: from, to: to, durationMs: request.durationMs)
        let initialWindows = windows(pid: app.processIdentifier, bundleId: app.bundleIdentifier)
        guard
            ([from] + points).allSatisfy({
                pointBelongsToApp($0, pid: app.processIdentifier, windows: initialWindows)
            })
        else {
            throw HelperError(
                code: .targetOutsideApp,
                message: "the drag path leaves the granted app")
        }
        guard
            let source = eventSource(request),
            let down = CGEvent(
                mouseEventSource: source, mouseType: .leftMouseDown,
                mouseCursorPosition: from, mouseButton: .left),
            let up = CGEvent(
                mouseEventSource: source, mouseType: .leftMouseUp,
                mouseCursorPosition: from, mouseButton: .left)
        else {
            throw HelperError(code: .operationFailed, message: "could not synthesize drag events")
        }
        let moves = try points.map { point -> CGEvent in
            guard
                let move = CGEvent(
                    mouseEventSource: source, mouseType: .leftMouseDragged,
                    mouseCursorPosition: point, mouseButton: .left)
            else {
                throw HelperError(
                    code: .operationFailed, message: "could not synthesize drag movement")
            }
            return move
        }
        var lastPoint = from
        let interval =
            Double(min(max(request.durationMs ?? 200, 0), 10_000)) / 1000 / Double(points.count)
        try ensureNotCancelled(request)
        try deliverDrag(
            count: moves.count,
            press: { try post(down, app: app, request: request) },
            move: { index in
                try ensureNotCancelled(request)
                try ensureNoSystemDialogFrontmost()
                guard
                    request.executionMode != .foreground
                        || NSWorkspace.shared.frontmostApplication?.processIdentifier
                            == app.processIdentifier
                else {
                    throw HelperError(
                        code: .yielded, message: "the granted app lost focus during drag")
                }
                try ensurePointInApp(points[index], app: app, request: request)
                moves[index].timestamp = DispatchTime.now().uptimeNanoseconds
                try post(moves[index], app: app, request: request)
                lastPoint = points[index]
            },
            release: {
                up.location = lastPoint
                up.timestamp = DispatchTime.now().uptimeNanoseconds
                try? post(up, app: app, request: request)
            },
            pause: { Thread.sleep(forTimeInterval: interval) })
        return Result(
            executionMode: request.executionMode ?? .background,
            success: true, usedFallback: true, detail: "posted drag with \(points.count) movements")
    }

    static func dragPoints(from: CGPoint, to: CGPoint, durationMs: Int?) -> [CGPoint] {
        let duration = min(max(durationMs ?? 200, 0), 10_000)
        let steps = max(20, min(1000, duration / 5))
        return (1...steps).map { step in
            let fraction = Double(step) / Double(steps)
            return CGPoint(
                x: from.x + (to.x - from.x) * fraction,
                y: from.y + (to.y - from.y) * fraction)
        }
    }

    /// Release at the last delivered point even when a later guard refuses.
    /// After the press, any failure has an uncertain outcome: releasing may
    /// complete a drop, so it must never be reported as a refusal before input.
    static func deliverDrag(
        count: Int, press: () throws -> Void, move: (Int) throws -> Void,
        release: () -> Void, pause: () -> Void
    ) throws {
        try press()
        defer { release() }
        do {
            for index in 0..<count {
                pause()
                try move(index)
            }
        } catch {
            let detail = (error as? HelperError)?.message ?? String(describing: error)
            throw HelperError(
                code: .operationFailed,
                message: "The drag stopped after input was sent: \(detail)")
        }
    }

    /// Resize one window of the granted app to a width/height in logical
    /// points. CGWindowID is transient; when a window_id was supplied it is
    /// re-validated against the app's current window list immediately before
    /// resizing.
    static func resizeWindow(_ request: HelperRequest) throws -> Result {
        try ensureNotCancelled(request)
        let app = try requireControllableApp(request)
        try ensureNoSystemDialogFrontmost()
        guard let width = request.width, let height = request.height,
            width.isFinite, height.isFinite, width > 0, height > 0,
            width <= 10_000, height <= 10_000
        else {
            throw HelperError(
                code: .invalidRequest, message: "resize_window requires positive width/height")
        }
        let appElement = AXTree.appElement(for: app.processIdentifier)
        let windows = Control.windows(
            pid: app.processIdentifier, bundleId: app.bundleIdentifier)
        guard !windows.isEmpty else {
            throw HelperError(code: .notFound, message: "app has no windows to resize")
        }
        let candidate: AXUIElement
        if let windowId = request.windowId {
            guard windows.contains(where: { $0.windowId == windowId }) else {
                throw HelperError(
                    code: .staleElement,
                    message: "window \(windowId) no longer exists for the granted app")
            }
            guard
                let windowElement = accessibleWindowElement(
                    matching: windowId, appElement: appElement)
            else {
                throw HelperError(
                    code: .staleElement,
                    message: "window \(windowId) has no accessibility resize handle")
            }
            candidate = windowElement
        } else {
            if let mainValue = AXTree.copyAttr(appElement, kAXMainWindowAttribute as CFString),
                CFGetTypeID(mainValue) == AXUIElementGetTypeID()
            {
                candidate = mainValue as! AXUIElement
            } else if let windowValue = AXTree.copyAttr(
                appElement, kAXFocusedWindowAttribute as CFString),
                CFGetTypeID(windowValue) == AXUIElementGetTypeID()
            {
                candidate = windowValue as! AXUIElement
            } else {
                throw HelperError(
                    code: .notFound, message: "app has no main/focused window to resize")
            }
        }
        var size = CGSize(width: width, height: height)
        guard let sizeValue = AXValueCreate(.cgSize, &size) else {
            throw HelperError(code: .operationFailed, message: "could not build size for resize")
        }
        try ensureNotCancelled(request)
        let error = AXUIElementSetAttributeValue(candidate, kAXSizeAttribute as CFString, sizeValue)
        guard error == .success else {
            throw HelperError(
                code: .operationFailed,
                message: "app rejected the resize (AX error \(error.rawValue))")
        }
        let deadline = ProcessInfo.processInfo.systemUptime + 1.0
        repeat {
            try ensureNotCancelled(request)
            if let frame = AXTree.copyFrame(candidate), sizeMatches(frame: frame, requested: size) {
                return Result(
                    executionMode: request.executionMode ?? .background, success: true,
                    usedFallback: false, detail: "verified resized window")
            }
            usleep(20_000)
        } while ProcessInfo.processInfo.systemUptime < deadline
        throw HelperError(
            code: .operationFailed, message: "the window did not reach the requested size")
    }

    static func sizeMatches(frame: AXTree.Frame, requested: CGSize) -> Bool {
        abs(frame.width - requested.width) <= 1 && abs(frame.height - requested.height) <= 1
    }

    /// Pure condition wait. Accessibility is requested for text conditions
    /// (they need the tree) but never synthesized input; app/window checks
    /// need no TCC grant. Always returns after the poll — a failing condition
    /// reports timed_out rather than blocking past its bound.
    static func waitCondition(_ request: HelperRequest) throws -> WaitResult {
        try ensureNotCancelled(request)
        let bundleId: String
        if let requested = request.bundleId {
            bundleId = requested
        } else {
            throw HelperError(code: .invalidRequest, message: "wait_condition requires bundle_id")
        }
        if isBlocked(bundleId) {
            throw HelperError(code: .operationFailed, message: "app \(bundleId) is not automatable")
        }
        guard let kind = request.condition else {
            throw HelperError(code: .invalidRequest, message: "wait_condition requires condition")
        }
        let requestedTimeout = request.timeoutSeconds ?? 10
        guard requestedTimeout.isFinite else {
            throw HelperError(code: .invalidRequest, message: "timeout_seconds must be finite")
        }
        let timeout = min(max(requestedTimeout, 0.1), 30)
        let deadline = ProcessInfo.processInfo.systemUptime + timeout
        var observed = false
        repeat {
            try ensureNotCancelled(request)
            switch kind {
            case .appRunning:
                observed = NSWorkspace.shared.runningApplications.contains(where: {
                    $0.bundleIdentifier == bundleId
                })
            case .windowVisible:
                observed = !Control.windows(pid: nil, bundleId: bundleId).isEmpty
            case .textPresent, .textAbsent:
                guard let text = request.text, !text.isEmpty else {
                    throw HelperError(
                        code: .invalidRequest, message: "text conditions require text")
                }
                let observation = try treeContains(
                    bundleId: bundleId, text: text, deadline: deadline, request: request)
                observed = observation.satisfies(kind)
            }
            let remaining = deadline - ProcessInfo.processInfo.systemUptime
            if !observed && remaining > 0 {
                Thread.sleep(forTimeInterval: min(remaining, 0.1))
            }
        } while !observed && ProcessInfo.processInfo.systemUptime < deadline
        return WaitResult(met: observed, timedOut: !observed)
    }

    /// Read the target element's role + label without acting — the broker's
    /// forced-confirmation tripwire classifies this before a control op runs.
    /// Resolves the element by its index-path id (the same path the control ops
    /// use) but deliberately does not enforce the fingerprint: it reports the
    /// element's current role/label so the broker classifies what is actually
    /// on screen now. Returns nulls when there is no addressed element or the
    /// path no longer resolves (the broker treats that as benign and fails
    /// open).
    static func describeElement(_ request: HelperRequest) throws -> DescribeResult {
        let app = try requireControllableApp(request)
        guard let elementId = request.elementId, !elementId.isEmpty else {
            return DescribeResult(role: nil, label: nil, fingerprint: nil)
        }
        let components = elementId.split(separator: ".").map(String.init)
        guard components.first == "0" else {
            return DescribeResult(role: nil, label: nil, fingerprint: nil)
        }
        var current = AXTree.appElement(for: app.processIdentifier)
        for raw in components.dropFirst() {
            guard let index = Int(raw) else {
                return DescribeResult(role: nil, label: nil, fingerprint: nil)
            }
            let children = AXTree.copyChildren(current)
            guard index >= 0, index < children.count else {
                return DescribeResult(role: nil, label: nil, fingerprint: nil)
            }
            current = children[index]
        }
        let role = AXTree.copyString(current, kAXRoleAttribute as CFString)
        let subrole = AXTree.copyString(current, kAXSubroleAttribute as CFString)
        let label =
            AXTree.copyString(current, kAXTitleAttribute as CFString)
            ?? AXTree.copyString(current, kAXDescriptionAttribute as CFString)
        let value = AXTree.copyValueString(current, kAXValueAttribute as CFString)
        let frame = AXTree.copyFrame(current)
        // Fold subrole into the role string so the cross-platform classifier
        // sees e.g. "AXSecureTextField" whether macOS exposes it as the role or
        // the subrole (the broker matches a "secure" substring).
        let combinedRole = [role, subrole].compactMap { $0 }.joined(separator: " ")
        // The live fingerprint binds a later confirmation to this exact element;
        // computed with the same accessors the act path re-checks against.
        let fingerprint = AXTree.fingerprint(
            role: role, title: label, hasValue: value != nil, frame: frame)
        return DescribeResult(
            role: combinedRole.isEmpty ? nil : combinedRole, label: label, fingerprint: fingerprint)
    }

    // MARK: - App resolution + blocklist

    private static func requireControllableApp(_ request: HelperRequest) throws
        -> NSRunningApplication
    {
        // Accessibility covers both AX actions and CGEvent input synthesis (no
        // Screen Recording needed for control). `requestAll` is a no-op once
        // granted — read_ax_tree has already prompted by this point.
        guard Permissions.requestAll().accessibility else {
            throw HelperError(
                code: .permissionDenied, message: "Accessibility permission is not granted")
        }
        guard let bundleId = request.bundleId, !bundleId.isEmpty else {
            throw HelperError(code: .invalidRequest, message: "control requires bundle_id")
        }
        guard !isBlocked(bundleId) else {
            throw HelperError(code: .operationFailed, message: "app \(bundleId) is not automatable")
        }
        guard
            let app = NSWorkspace.shared.runningApplications.first(where: {
                $0.bundleIdentifier == bundleId
            })
        else {
            throw HelperError(code: .notFound, message: "app \(bundleId) is not running")
        }
        return app
    }

    // MARK: - Element re-resolution + stale detection

    /// Re-walk the live AX tree to the element named by `request.element_id`
    /// (an index path from the app root) and, if a fingerprint was supplied,
    /// verify the element's identity has not drifted. Throws `stale_element`
    /// when the path no longer resolves or the fingerprint changed.
    private static func resolveElement(app: NSRunningApplication, request: HelperRequest) throws
        -> AXUIElement
    {
        try resolveElement(
            app: app, elementId: request.elementId, elementFingerprint: request.elementFingerprint)
    }

    private static func resolveElement(
        app: NSRunningApplication, elementId: String?, elementFingerprint: String?
    ) throws -> AXUIElement {
        guard let elementId, !elementId.isEmpty else {
            throw HelperError(code: .invalidRequest, message: "element_id is required")
        }
        let components = elementId.split(separator: ".").map(String.init)
        guard components.first == "0" else {
            throw HelperError(
                code: .invalidRequest, message: "malformed element_id: \(elementId)")
        }

        var current = AXTree.appElement(for: app.processIdentifier)
        for raw in components.dropFirst() {
            guard let index = Int(raw) else {
                throw HelperError(
                    code: .invalidRequest, message: "malformed element_id: \(elementId)")
            }
            let children = AXTree.copyChildren(current)
            guard index >= 0, index < children.count else {
                throw HelperError(
                    code: .staleElement,
                    message:
                        "element \(elementId) no longer exists; re-read the app content and retry")
            }
            current = children[index]
        }

        if let expected = elementFingerprint {
            let role = AXTree.copyString(current, kAXRoleAttribute as CFString)
            let title =
                AXTree.copyString(current, kAXTitleAttribute as CFString)
                ?? AXTree.copyString(current, kAXDescriptionAttribute as CFString)
            let value = AXTree.copyValueString(current, kAXValueAttribute as CFString)
            let frame = AXTree.copyFrame(current)
            let actual = AXTree.fingerprint(
                role: role, title: title, hasValue: value != nil, frame: frame)
            guard actual == expected else {
                throw HelperError(
                    code: .staleElement,
                    message:
                        "element \(elementId) changed since it was read; re-read the app content and retry"
                )
            }
        }
        return current
    }

    private static func elementCenter(_ element: AXUIElement) -> CGPoint? {
        guard let frame = AXTree.copyFrame(element) else { return nil }
        return CGPoint(x: frame.x + frame.width / 2, y: frame.y + frame.height / 2)
    }

    private static func explicitPoint(_ request: HelperRequest) -> CGPoint? {
        guard let x = request.x, let y = request.y else { return nil }
        return CGPoint(x: x, y: y)
    }

    /// App-owned on-screen windows. `pid != nil` filters the exact process;
    /// `bundleId != nil` filters by bundle id. This is the same CGWindowList
    /// source used by confinement, so resize validation and coordinate
    /// confinement see the same transient reality.
    private static func windows(pid: pid_t?, bundleId: String?) -> [AppWindow] {
        guard
            let infoList = CGWindowListCopyWindowInfo(
                [.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
                as? [[String: Any]]
        else { return [] }
        let bundleByPid = Dictionary(
            NSWorkspace.shared.runningApplications.compactMap { app in
                app.bundleIdentifier.map { (app.processIdentifier, $0) }
            },
            uniquingKeysWith: { first, _ in first })
        var result: [AppWindow] = []
        for info in infoList {
            guard
                let ownerPid = (info[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value,
                let wid = (info[kCGWindowNumber as String] as? NSNumber)?.uint32Value
            else { continue }
            if let pid, ownerPid != pid { continue }
            if let bundleId, bundleByPid[ownerPid] != bundleId { continue }
            guard let bounds = info[kCGWindowBounds as String] as? [String: Any],
                (info[kCGWindowAlpha as String] as? NSNumber)?.doubleValue ?? 1 > 0,
                (bounds["Width"] as? NSNumber)?.doubleValue ?? 0 > 0,
                (bounds["Height"] as? NSNumber)?.doubleValue ?? 0 > 0
            else { continue }
            result.append(
                AppWindow(
                    windowId: wid, ownerPid: ownerPid,
                    frame: CGRect(
                        x: (bounds["X"] as? NSNumber)?.doubleValue ?? 0,
                        y: (bounds["Y"] as? NSNumber)?.doubleValue ?? 0,
                        width: (bounds["Width"] as? NSNumber)?.doubleValue ?? 0,
                        height: (bounds["Height"] as? NSNumber)?.doubleValue ?? 0)))
        }
        return result
    }

    /// Find the AX window element whose screen frame matches `windowId`'s
    /// current CGWindow bounds, rounded to whole points to survive Retina
    /// jitter. Returns nil when the AX list has no matching window (many apps
    /// expose only some windows through AX).
    private static func accessibleWindowElement(
        matching windowId: UInt32, appElement: AXUIElement
    ) -> AXUIElement? {
        guard
            let target = Control.windows(pid: nil, bundleId: nil)
                .first(where: { $0.windowId == windowId })
        else { return nil }
        let wanted = CGRect(
            x: target.frame.origin.x.rounded(),
            y: target.frame.origin.y.rounded(),
            width: target.frame.width.rounded(),
            height: target.frame.height.rounded())
        let value = AXTree.copyAttr(appElement, kAXWindowsAttribute as CFString)
        let windows = (value as? [AXUIElement]) ?? []
        return windows.first { windowElement in
            guard let frame = AXTree.copyFrame(windowElement) else { return false }
            return CGRect(
                x: frame.x.rounded(), y: frame.y.rounded(),
                width: frame.width.rounded(), height: frame.height.rounded()) == wanted
        }
    }

    enum TextObservation: Equatable {
        case present, absent, incomplete

        func satisfies(_ condition: WaitConditionKind) -> Bool {
            switch condition {
            case .textPresent: return self == .present
            case .textAbsent: return self == .absent
            default: return false
            }
        }
    }

    struct TextNode<Element> {
        let strings: [String]
        let children: [Element]
        let complete: Bool
    }

    /// Absence requires a complete tree. An unreadable node or exhausted bound
    /// leaves the condition unmet, even when no text matched the readable nodes.
    static func searchText<Element>(
        root: Element, text: String, maxNodes: Int = 2000,
        maxDepth: Int = 25, deadline: Double, now: () -> Double,
        read: (Element) throws -> TextNode<Element>
    ) rethrows -> TextObservation {
        var pending = [(root, 0)]
        var remaining = maxNodes
        var complete = true
        while let (element, depth) = pending.popLast() {
            guard remaining > 0, now() < deadline else { return .incomplete }
            if depth > maxDepth {
                complete = false
                continue
            }
            remaining -= 1
            let node = try read(element)
            if node.strings.contains(where: { $0.contains(text) }) { return .present }
            complete = complete && node.complete
            pending.append(contentsOf: node.children.reversed().map { ($0, depth + 1) })
        }
        return complete ? .absent : .incomplete
    }

    private static func treeContains(
        bundleId: String, text: String, deadline: Double, request: HelperRequest
    ) throws -> TextObservation {
        guard Permissions.requestAll().accessibility else {
            throw HelperError(
                code: .permissionDenied, message: "Accessibility permission is not granted")
        }
        guard
            let app = NSWorkspace.shared.runningApplications.first(where: {
                $0.bundleIdentifier == bundleId
            })
        else { return .incomplete }
        return try searchText(
            root: AXTree.appElement(for: app.processIdentifier), text: text,
            deadline: deadline, now: { ProcessInfo.processInfo.systemUptime },
            read: {
                try ensureNotCancelled(request)
                return readTextNode($0, deadline: deadline)
            })
    }

    private static func readTextNode(_ element: AXUIElement, deadline: Double) -> TextNode<
        AXUIElement
    > {
        var complete = true
        func attribute(_ name: CFString) -> CFTypeRef? {
            let remaining = deadline - ProcessInfo.processInfo.systemUptime
            guard remaining > 0 else {
                complete = false
                return nil
            }
            AXUIElementSetMessagingTimeout(element, Float(min(remaining, 0.2)))
            var value: CFTypeRef?
            let error = AXUIElementCopyAttributeValue(element, name, &value)
            if error == .attributeUnsupported || error == .noValue { return nil }
            guard error == .success else {
                complete = false
                return nil
            }
            return value
        }
        var strings: [String] = []
        for name in [kAXTitleAttribute, kAXDescriptionAttribute, kAXValueAttribute] {
            guard let value = attribute(name as CFString) else { continue }
            if CFGetTypeID(value) == CFStringGetTypeID() {
                strings.append(value as! String)
            } else if let number = value as? NSNumber {
                strings.append(number.stringValue)
            }
        }
        var children: [AXUIElement] = []
        if let value = attribute(kAXChildrenAttribute as CFString) {
            if let array = value as? [AXUIElement] {
                children = array
            } else {
                complete = false
            }
        }
        return TextNode(strings: strings, children: children, complete: complete)
    }

    /// Resolve a target (element center, else explicit global point) to a
    /// point, applying the app-confinement check to a raw coordinate. `prefix`
    /// selects `from_*`/`to_*` drag fields; the single-target fields are used
    /// when `prefix` is nil.
    private static func resolveTargetPoint(
        app: NSRunningApplication, request: HelperRequest, prefix: String? = nil
    ) throws -> CGPoint {
        let elementId =
            prefix.map {
                $0 == "to" ? request.toElementId : request.fromElementId
            } ?? request.elementId
        let fingerprint =
            prefix.map {
                $0 == "to" ? request.toElementFingerprint : request.fromElementFingerprint
            } ?? request.elementFingerprint
        let x = prefix.map { $0 == "to" ? request.toX : request.fromX } ?? request.x
        let y = prefix.map { $0 == "to" ? request.toY : request.fromY } ?? request.y

        if elementId != nil {
            let element = try resolveElement(
                app: app,
                elementId: elementId,
                elementFingerprint: fingerprint)
            guard let center = elementCenter(element) else {
                throw HelperError(
                    code: .operationFailed, message: "element has no on-screen frame to target")
            }
            return center
        }
        guard let x, let y else {
            throw HelperError(
                code: .invalidRequest, message: "target requires element_id or x/y")
        }
        let point = CGPoint(x: x, y: y)
        try ensurePointInApp(point, app: app, request: request)
        return point
    }

    /// Hit-test through Accessibility to ignore click-through overlays while
    /// refusing any pointer target owned by another app.
    private static func ensurePointInApp(
        _ point: CGPoint, app: NSRunningApplication, request: HelperRequest
    ) throws {
        if request.executionMode != .foreground {
            guard
                pointBelongsToApp(
                    point, pid: app.processIdentifier,
                    windows: windows(pid: app.processIdentifier, bundleId: app.bundleIdentifier))
            else {
                throw HelperError(
                    code: .targetOutsideApp, message: "the target point is outside the granted app")
            }
            return
        }
        guard point.x.isFinite, point.y.isFinite else {
            throw HelperError(code: .invalidRequest, message: "target coordinates must be finite")
        }
        var hit: AXUIElement?
        let system = AXUIElementCreateSystemWide()
        AXUIElementSetMessagingTimeout(system, 0.2)
        let error = AXUIElementCopyElementAtPosition(
            system, Float(point.x), Float(point.y), &hit)
        var owner: pid_t = 0
        guard error == .success, let hit,
            AXUIElementGetPid(hit, &owner) == .success,
            owner == app.processIdentifier
        else {
            throw HelperError(
                code: .targetOutsideApp,
                message:
                    "the pointer target is outside the granted app (AX \(error.rawValue), owner \(owner))"
            )
        }
    }

    /// CGWindowList returns windows from front to back.
    static func pointBelongsToApp(_ point: CGPoint, pid: pid_t, windows: [AppWindow]) -> Bool {
        guard point.x.isFinite, point.y.isFinite else { return false }
        return windows.first(where: { $0.frame.contains(point) })?.ownerPid == pid
    }

    static func requireForeground(_ request: HelperRequest, operation: String) throws {
        guard request.executionMode == .foreground else {
            throw HelperError(
                code: .requiresForeground,
                message: "\(operation) requires foreground computer use; no input was sent")
        }
    }

    private static func eventSource(_ request: HelperRequest) -> CGEventSource? {
        CGEventSource(stateID: .combinedSessionState)
    }

    private static func post(_ event: CGEvent, app: NSRunningApplication, request: HelperRequest)
        throws
    {
        // Every synthesized-input entry point rejects background before it reaches this function.
        precondition(request.executionMode == .foreground)
        try InputRecovery.post(event, request: request)
    }

    private static func resolveBackgroundPoint(app: NSRunningApplication, request: HelperRequest)
        throws -> AXUIElement
    {
        guard let point = explicitPoint(request), point.x.isFinite, point.y.isFinite else {
            throw HelperError(
                code: .invalidRequest, message: "background click requires an element or finite x/y"
            )
        }
        let application = AXTree.appElement(for: app.processIdentifier)
        var element: AXUIElement?
        let error = AXUIElementCopyElementAtPosition(
            application, Float(point.x), Float(point.y), &element)
        var owner: pid_t = 0
        guard error == .success, let element, AXUIElementGetPid(element, &owner) == .success,
            owner == app.processIdentifier
        else {
            throw HelperError(
                code: .requiresForeground,
                message: "the coordinate has no background Accessibility target")
        }
        return element
    }

    static func backgroundScrollPosition(current: Double, delta: Double) -> Double {
        min(1, max(0, current + (delta > 0 ? 0.1 : -0.1)))
    }

    private static func scrollAccessibility(app: NSRunningApplication, request: HelperRequest)
        throws -> Result
    {
        let dx = request.dx ?? 0
        let dy = request.dy ?? 0
        guard dx.isFinite, dy.isFinite else {
            throw HelperError(code: .invalidRequest, message: "scroll deltas must be finite")
        }
        guard request.elementId != nil else {
            throw HelperError(
                code: .requiresForeground,
                message: "background scrolling requires an Accessibility scroll element")
        }
        var element = try resolveElement(app: app, request: request)
        for _ in 0..<25 {
            if AXTree.copyString(element, kAXRoleAttribute as CFString) == kAXScrollAreaRole {
                break
            }
            guard let parent = AXTree.copyAttr(element, kAXParentAttribute as CFString),
                CFGetTypeID(parent) == AXUIElementGetTypeID()
            else { break }
            element = parent as! AXUIElement
        }
        var actions: [(AXUIElement, CFString?, Double?)] = []
        for (delta, attribute) in [
            (dx, kAXHorizontalScrollBarAttribute), (dy, kAXVerticalScrollBarAttribute),
        ] {
            if delta == 0 { continue }
            guard let value = AXTree.copyAttr(element, attribute as CFString),
                CFGetTypeID(value) == AXUIElementGetTypeID()
            else {
                throw HelperError(
                    code: .requiresForeground,
                    message: "the scroll element has no background scrollbar")
            }
            let bar = value as! AXUIElement
            let action = (delta > 0 ? kAXIncrementAction : kAXDecrementAction) as CFString
            var names: CFArray?
            if AXUIElementCopyActionNames(bar, &names) == .success,
                (names as? [String])?.contains(action as String) == true
            {
                actions.append((bar, action, nil))
                continue
            }
            var settable = DarwinBoolean(false)
            guard
                AXUIElementIsAttributeSettable(bar, kAXValueAttribute as CFString, &settable)
                    == .success,
                settable.boolValue,
                let current = AXTree.copyAttr(bar, kAXValueAttribute as CFString) as? NSNumber,
                current.doubleValue >= 0, current.doubleValue <= 1
            else {
                throw HelperError(
                    code: .requiresForeground,
                    message: "the scrollbar does not support background scrolling")
            }
            let next = backgroundScrollPosition(current: current.doubleValue, delta: delta)
            actions.append((bar, nil, next))
        }
        for (bar, action, value) in actions {
            try ensureNotCancelled(request)
            if let action {
                guard AXUIElementPerformAction(bar, action) == .success else {
                    throw HelperError(
                        code: .operationFailed,
                        message: "the scrollbar rejected its Accessibility action")
                }
            } else if let value {
                guard
                    AXUIElementSetAttributeValue(
                        bar, kAXValueAttribute as CFString, NSNumber(value: value)) == .success,
                    let actual = AXTree.copyAttr(bar, kAXValueAttribute as CFString) as? NSNumber,
                    abs(actual.doubleValue - value) < 0.001
                else {
                    throw HelperError(
                        code: .operationFailed,
                        message: "the scrollbar did not retain its requested position")
                }
            }
        }
        return Result(
            executionMode: .background, success: true, usedFallback: false,
            detail: "AX scrollbar step; the app determines the scroll distance")
    }

    private static func prepareForInput(_ app: NSRunningApplication, request: HelperRequest) throws
    {
        try ensureNotCancelled(request)
        if request.executionMode == .foreground { try activateAndWait(app, request: request) }
    }

    // MARK: - Activation

    /// Upper bound on waiting for a freshly-activated app to become frontmost
    /// before synthesizing input.
    private static let activationTimeout: TimeInterval = 0.6

    /// `NSRunningApplication.activate()` is asynchronous — it returns before the
    /// app is actually frontmost. Synthesized input posted immediately races
    /// that activation: the leading keystrokes land in the previously-frontmost
    /// app (or are dropped) and only the tail reaches the target, which is
    /// exactly the "only a couple characters get typed" symptom. Activate, then
    /// spin the run loop until the workspace reports the target frontmost.
    /// Refuse input if the granted app does not
    /// become frontmost before the bound.
    private static func activateAndWait(_ app: NSRunningApplication, request: HelperRequest) throws
    {
        try ensureNotCancelled(request)
        app.activate()
        try waitForActivation(
            timeout: activationTimeout,
            isFrontmost: {
                NSWorkspace.shared.frontmostApplication?.processIdentifier == app.processIdentifier
            },
            check: {
                try ensureNotCancelled(request)
                try ensureNoSystemDialogFrontmost()
            },
            now: { ProcessInfo.processInfo.systemUptime },
            pause: {
                RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
                usleep(5_000)
            })
    }

    static func waitForActivation(
        timeout: TimeInterval, isFrontmost: () -> Bool,
        check: () throws -> Void, now: () -> TimeInterval, pause: () -> Void
    ) throws {
        let deadline = now() + timeout
        // isActive can change before the workspace's frontmost PID. Wait for
        // the same observation that guards input after approval UI closes.
        while !isFrontmost(), now() < deadline {
            try check()
            pause()
        }
        try check()
        guard isFrontmost() else {
            throw HelperError(code: .yielded, message: "the granted app did not become frontmost")
        }
    }

    // MARK: - CGEvent synthesis

    private static func postMouseClick(
        at point: CGPoint, button: String, clickCount: Int, app: NSRunningApplication,
        request: HelperRequest
    ) throws {
        try postMouseSequence(
            at: point, button: button, clickCount: clickCount, app: app, request: request)
    }

    /// Post a single-target click sequence with the same combined session
    /// source hover/drag use, so Electron-style apps see mouse state coherently.
    private static func postMouseSequence(
        at point: CGPoint, button: String, clickCount: Int, app: NSRunningApplication,
        request: HelperRequest
    ) throws {
        let (downType, upType, cgButton): (CGEventType, CGEventType, CGMouseButton) =
            button == "right"
            ? (.rightMouseDown, .rightMouseUp, .right)
            : (.leftMouseDown, .leftMouseUp, .left)
        let count = max(1, min(clickCount, 3))
        let source = eventSource(request)
        for click in 1...count {
            try ensureNotCancelled(request)
            guard
                let down = CGEvent(
                    mouseEventSource: source, mouseType: downType, mouseCursorPosition: point,
                    mouseButton: cgButton),
                let up = CGEvent(
                    mouseEventSource: source, mouseType: upType, mouseCursorPosition: point,
                    mouseButton: cgButton)
            else {
                throw HelperError(
                    code: .operationFailed, message: "could not synthesize mouse event")
            }
            if count > 1 {
                down.setIntegerValueField(.mouseEventClickState, value: Int64(click))
                up.setIntegerValueField(.mouseEventClickState, value: Int64(click))
            }
            try post(down, app: app, request: request)
            try post(up, app: app, request: request)
        }
    }

    /// How long a synthesized key chord is held down before release (~18ms).
    /// Long enough to span a run-loop tick so apps that sample key state per
    /// tick register the press; short enough to stay imperceptible. Without it,
    /// a zero-duration down→up is intermittently missed (notably bare Return
    /// and command-modified shortcuts).
    private static let keyPressHoldMicros: useconds_t = 18_000

    /// Inter-keystroke gap for synthesized typing (~4ms). Posting Unicode
    /// keystrokes to the HID tap faster than the target app drains its event
    /// queue makes the system coalesce/drop events — the other cause (besides
    /// the activation race) of truncated typing. A few ms keeps the stream
    /// intact while staying imperceptible; even a 1000-char paste is ~4s, well
    /// under the broker's 30s helper timeout.
    private static let perKeystrokeDelayMicros: useconds_t = 4_000

    /// Hard cap on the UTF-16 units the synthesized-keystroke fallback will
    /// type. At `perKeystrokeDelayMicros` per unit (plus per-event overhead), a
    /// longer burst would blow past the broker's 30s helper timeout and be
    /// killed mid-type, leaving partially-entered text in the user's app.
    /// Failing cleanly here lets the agent chunk the input. The atomic AXValue
    /// path (`typeText`) is unbounded; this only caps the fallback used when a
    /// field rejects a direct value set. 4000 × ~4ms ≈ 16s, comfortably under
    /// the timeout even after the activation wait.
    private static let maxSynthesizedTypeUnits = 4_000

    /// Type arbitrary text by posting per-grapheme Unicode keystrokes (works
    /// regardless of keyboard layout). Each grapheme cluster — a base character
    /// plus its combining marks and any surrogate pair — is posted as one event
    /// carrying all of its UTF-16 units, so a non-BMP character (an emoji, CJK
    /// ext-B) is never split into a lone, unpaired surrogate that the receiving
    /// app would drop or replace with U+FFFD.
    private static func typeUnicode(
        _ text: String, app: NSRunningApplication, request: HelperRequest
    ) throws {
        let units = Array(text.utf16)
        guard units.count <= maxSynthesizedTypeUnits else {
            throw HelperError(
                code: .invalidRequest,
                message:
                    "text too long for synthesized typing (\(units.count) chars, max \(maxSynthesizedTypeUnits)); the field rejected a direct value set — type it in smaller chunks"
            )
        }
        // One event source for the whole burst gives steadier ordering than a
        // fresh nil source per event.
        let source = eventSource(request)
        for grapheme in text {
            try ensureNotCancelled(request)
            try ensureNoSystemDialogFrontmost()
            guard
                request.executionMode != .foreground
                    || NSWorkspace.shared.frontmostApplication?.bundleIdentifier == request.bundleId
            else {
                throw HelperError(
                    code: .yielded, message: "the granted app lost focus while typing")
            }
            var cluster = Array(grapheme.utf16)
            guard
                let down = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: true),
                let up = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: false)
            else {
                throw HelperError(code: .operationFailed, message: "could not synthesize keystroke")
            }
            let length = cluster.count
            cluster.withUnsafeMutableBufferPointer { buffer in
                down.keyboardSetUnicodeString(
                    stringLength: length, unicodeString: buffer.baseAddress!)
                up.keyboardSetUnicodeString(
                    stringLength: length, unicodeString: buffer.baseAddress!)
            }
            try post(down, app: app, request: request)
            try post(up, app: app, request: request)
            usleep(perKeystrokeDelayMicros)
        }
    }

    /// A chord modifier resolved to the pieces a synthesized press needs: the
    /// modifier's virtual key code (so the real key can be pressed, not just
    /// the flag) and its `CGEventFlags` bit.
    private struct ResolvedModifier {
        let keyCode: CGKeyCode
        let flag: CGEventFlags
    }

    /// Map the requested modifier names to their `(keyCode, flag)` pairs, in
    /// request order and de-duplicated. Virtual key codes are the left-hand
    /// modifier keys (hard-coded to avoid a Carbon import). Unknown names are
    /// ignored.
    private static func resolveModifiers(_ modifiers: [String]) -> [ResolvedModifier] {
        var resolved: [ResolvedModifier] = []
        var seen = Set<CGKeyCode>()
        for modifier in modifiers {
            let pair: (CGKeyCode, CGEventFlags)?
            switch modifier.lowercased() {
            case "cmd", "command", "meta": pair = (0x37, .maskCommand)
            case "shift": pair = (0x38, .maskShift)
            case "ctrl", "control": pair = (0x3B, .maskControl)
            case "alt", "option", "opt": pair = (0x3A, .maskAlternate)
            case "fn", "function": pair = (0x3F, .maskSecondaryFn)
            default: pair = nil
            }
            guard let (keyCode, flag) = pair, seen.insert(keyCode).inserted else { continue }
            resolved.append(ResolvedModifier(keyCode: keyCode, flag: flag))
        }
        return resolved
    }

    /// Map a key name to its macOS virtual key code (the `kVK_*` constants,
    /// hard-coded to avoid a Carbon import). Covers the keys an agent
    /// realistically needs: letters, digits, common punctuation, and the named
    /// navigation/editing keys. Case-insensitive; a single character resolves
    /// to its base key.
    private static func virtualKeyCode(for key: String) -> CGKeyCode? {
        let lower = key.lowercased()
        if let named = namedKeyCodes[lower] { return named }
        // A single character → its base ANSI key (modifiers like shift are
        // applied separately).
        if lower.count == 1, let code = characterKeyCodes[Character(lower)] { return code }
        return nil
    }

    private static let namedKeyCodes: [String: CGKeyCode] = [
        "return": 0x24, "enter": 0x24,
        "tab": 0x30,
        "space": 0x31, "spacebar": 0x31,
        "delete": 0x33, "backspace": 0x33,
        "forwarddelete": 0x75,
        "escape": 0x35, "esc": 0x35,
        "left": 0x7B, "right": 0x7C, "down": 0x7D, "up": 0x7E,
        "home": 0x73, "end": 0x77, "pageup": 0x74, "pagedown": 0x79,
        "f1": 0x7A, "f2": 0x78, "f3": 0x63, "f4": 0x76, "f5": 0x60, "f6": 0x61,
        "f7": 0x62, "f8": 0x64, "f9": 0x65, "f10": 0x6D, "f11": 0x67, "f12": 0x6F,
    ]

    private static let characterKeyCodes: [Character: CGKeyCode] = [
        "a": 0x00, "s": 0x01, "d": 0x02, "f": 0x03, "h": 0x04, "g": 0x05, "z": 0x06, "x": 0x07,
        "c": 0x08, "v": 0x09, "b": 0x0B, "q": 0x0C, "w": 0x0D, "e": 0x0E, "r": 0x0F, "y": 0x10,
        "t": 0x11, "1": 0x12, "2": 0x13, "3": 0x14, "4": 0x15, "6": 0x16, "5": 0x17, "=": 0x18,
        "9": 0x19, "7": 0x1A, "-": 0x1B, "8": 0x1C, "0": 0x1D, "]": 0x1E, "o": 0x1F, "u": 0x20,
        "[": 0x21, "i": 0x22, "p": 0x23, "l": 0x25, "j": 0x26, "'": 0x27, "k": 0x28, ";": 0x29,
        "\\": 0x2A, ",": 0x2B, "/": 0x2C, "n": 0x2D, "m": 0x2E, ".": 0x2F, "`": 0x32,
    ]
}
