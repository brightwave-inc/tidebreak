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
/// broker cannot drive a terminal, OpenWave itself, or the system auth/login
/// surfaces.
enum Control {
    /// Returned to the broker for every control op. `usedFallback` is true when
    /// AX targeting was not available and a coordinate/keystroke synthesis was
    /// used instead — useful signal for the agent.
    struct Result: Encodable {
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

    // MARK: - Never-automate blocklist (defensive copy; the broker is authoritative)

    /// Bundle ids (exact, or as a dotted prefix) the helper will never act on.
    /// Mirrors the broker's blocklist — defense in depth, not the primary gate.
    static let blockedBundlePrefixes: [String] = [
        // Never automate OpenWave itself (covers the desktop and any
        // channel-suffixed variant).
        "io.brightwave.openwave",
        "io.brightwave.",
        // Terminals are arbitrary code execution outside the broker's
        // confinement.
        "com.apple.Terminal",
        "com.googlecode.iterm2",
        // Auth / login / credential surfaces — driving these could defeat a
        // human-in-the-loop check.
        "com.apple.loginwindow",
        "com.apple.SecurityAgent",
        "com.apple.systempreferences",
        "com.apple.keychainaccess",
    ]

    /// Matches the broker's semantics exactly: an entry blocks its exact id and
    /// anything nested under it at a dotted boundary, so `com.apple.Terminal`
    /// blocks `com.apple.Terminal.helper` but not `com.apple.Terminalized`.
    static func isBlocked(_ bundleId: String) -> Bool {
        blockedBundlePrefixes.contains { entry in
            let base = entry.hasSuffix(".") ? String(entry.dropLast()) : entry
            return bundleId == base
                || (bundleId.hasPrefix(base) && bundleId.dropFirst(base.count).hasPrefix("."))
        }
    }

    /// Throw `operation_failed` when a request targets a blocked bundle. Shared
    /// by every op that names an app — capture and read as well as control, so
    /// a compromised broker cannot read Terminal or OpenWave through the helper
    /// directly.
    static func ensureNotBlocked(_ bundleId: String?) throws {
        if let bundleId, isBlocked(bundleId) {
            throw HelperError(
                code: .operationFailed, message: "app \(bundleId) is not automatable")
        }
    }

    // MARK: - Auto-yield on system security dialogs

    /// Bundle ids whose presence as the frontmost app means a system security /
    /// authorization surface owns the screen — a TCC / admin-password / "wants
    /// to control your computer" prompt, the login/unlock screen, or a Touch ID
    /// / local-auth panel. These are modal, focus-stealing, and exactly where
    /// the user is mid-authentication, so synthesizing input could drive a
    /// click or keystroke straight into the one surface the agent must never
    /// touch. Distinct from `blockedBundlePrefixes` (which refuses a target
    /// app): this refuses to act at all while one of these owns the foreground,
    /// whatever the target.
    private static let systemDialogFrontmostPrefixes: [String] = [
        "com.apple.SecurityAgent",
        "com.apple.loginwindow",
        "com.apple.CoreAuthUI",
        "com.apple.coreauthd",
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
        if systemDialogFrontmostPrefixes.contains(where: { frontmost == $0 || frontmost.hasPrefix($0) })
        {
            throw HelperError(
                code: .yielded,
                message:
                    "a system security dialog (\(frontmost)) is in the foreground; stopped instead of synthesizing input into it"
            )
        }
    }

    // MARK: - Ops

    static func click(_ request: HelperRequest) throws -> Result {
        let app = try requireControllableApp(request)
        try ensureNoSystemDialogFrontmost()
        let button = request.button ?? "left"
        let count = request.clickCount ?? 1

        if request.elementId != nil {
            let element = try resolveElement(app: app, request: request)
            // AXPress is the clean path for a plain single left click — no
            // coordinates, no Retina math.
            if button == "left", count <= 1 {
                if AXUIElementPerformAction(element, kAXPressAction as CFString) == .success {
                    return Result(success: true, usedFallback: false, detail: "AXPress")
                }
            }
            // Right / double / AXPress-unsupported → synthesize at the element's
            // on-screen center.
            guard let center = elementCenter(element) else {
                throw HelperError(
                    code: .operationFailed, message: "element has no on-screen frame to click")
            }
            try postMouseClick(at: center, button: button, clickCount: count)
            return Result(success: true, usedFallback: true, detail: "synthesized \(button) click")
        }

        guard let point = explicitPoint(request) else {
            throw HelperError(code: .invalidRequest, message: "click requires element_id or x/y")
        }
        // A raw point is global; confine it to the granted app's windows so a
        // click cannot land on another app's (or OpenWave's own) surface.
        try ensurePointInApp(point, app: app)
        try postMouseClick(at: point, button: button, clickCount: count)
        return Result(success: true, usedFallback: true, detail: "synthesized click at point")
    }

    static func typeText(_ request: HelperRequest) throws -> Result {
        let app = try requireControllableApp(request)
        try ensureNoSystemDialogFrontmost()
        guard let text = request.text else {
            throw HelperError(code: .invalidRequest, message: "type_text requires text")
        }

        if request.elementId != nil {
            let element = try resolveElement(app: app, request: request)
            AXUIElementSetAttributeValue(
                element, kAXFocusedAttribute as CFString, true as CFTypeRef)
            // Setting AXValue is the clean path for a text field; if the element
            // rejects it (not a value-bearing role), fall back to synthesizing
            // keystrokes into the now-focused element.
            if AXUIElementSetAttributeValue(element, kAXValueAttribute as CFString, text as CFString)
                == .success
            {
                return Result(success: true, usedFallback: false, detail: "set AXValue")
            }
            // Synthesized keystrokes go to the frontmost app, not the element's
            // PID, and setting AXFocused does not reliably foreground the app —
            // bring it forward first (as keyPress and the no-element path below
            // do) so the text cannot land in a different window.
            activateAndWait(app)
            try typeUnicode(text)
            return Result(success: true, usedFallback: true, detail: "synthesized keystrokes")
        }

        activateAndWait(app)
        try typeUnicode(text)
        return Result(success: true, usedFallback: true, detail: "synthesized keystrokes")
    }

    static func keyPress(_ request: HelperRequest) throws -> Result {
        let app = try requireControllableApp(request)
        try ensureNoSystemDialogFrontmost()
        guard let keyName = request.key else {
            throw HelperError(code: .invalidRequest, message: "key_press requires key")
        }
        guard let keyCode = virtualKeyCode(for: keyName) else {
            throw HelperError(code: .invalidRequest, message: "unknown key: \(keyName)")
        }
        // A chord targets the focused app, so bring it forward first.
        activateAndWait(app)
        // A real event source (not nil) delivers far more reliably than a fresh
        // nil source. With a nil source, apps that read modifier/key state from
        // the session (Electron, browsers) intermittently drop synthesized
        // chords and the bare Return that submits a field.
        let source = CGEventSource(stateID: .combinedSessionState)
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

        // Everything is synthesized; from here only posting happens, which
        // cannot fail.
        for modDown in modifierDowns {
            modDown.post(tap: .cghidEventTap)
        }
        // Let the flagsChanged events land before the keystroke so the modifier
        // state is settled.
        if !modifiers.isEmpty {
            usleep(keyPressHoldMicros)
        }
        // Hold the key briefly before release. A zero-duration down→up can land
        // inside a single run-loop tick and read as "never pressed" to apps that
        // sample key state per tick; a short hold makes the press register as a
        // real keystroke.
        down.post(tap: .cghidEventTap)
        usleep(keyPressHoldMicros)
        up.post(tap: .cghidEventTap)
        // Release modifiers last, after the key, while the chord flags are still
        // asserted.
        for modUp in modifierUps {
            modUp.post(tap: .cghidEventTap)
        }
        return Result(success: true, usedFallback: false, detail: "key \(keyName)")
    }

    static func scroll(_ request: HelperRequest) throws -> Result {
        let app = try requireControllableApp(request)
        try ensureNoSystemDialogFrontmost()
        let dx = request.dx ?? 0
        let dy = request.dy ?? 0

        // A scroll-wheel event lands on the view under the cursor, so position
        // the cursor over the target first (element center, else an explicit
        // point) when one was given.
        var usedFallback = true
        if request.elementId != nil {
            let element = try resolveElement(app: app, request: request)
            if let center = elementCenter(element) {
                CGWarpMouseCursorPosition(center)
            }
            usedFallback = false
        } else if let point = explicitPoint(request) {
            // A raw point is global; confine it to the granted app's windows.
            try ensurePointInApp(point, app: app)
            CGWarpMouseCursorPosition(point)
        }

        // The deltas feed a CGEvent's Int32 wheel fields; a finite but huge
        // delta (a model asking to "scroll to the bottom" with dy=1e10) would
        // trap the Int32 conversion and crash the process. Clamp instead of
        // crashing — an enormous scroll is an enormous scroll.
        let clampedDy = min(max(dy.rounded(), Double(Int32.min)), Double(Int32.max))
        let clampedDx = min(max(dx.rounded(), Double(Int32.min)), Double(Int32.max))
        guard
            let event = CGEvent(
                scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 2,
                wheel1: Int32(clampedDy), wheel2: Int32(clampedDx), wheel3: 0)
        else {
            throw HelperError(code: .operationFailed, message: "could not synthesize scroll event")
        }
        event.post(tap: .cghidEventTap)
        return Result(success: true, usedFallback: usedFallback, detail: "scroll dx \(dx) dy \(dy)")
    }

    static func focusWindow(_ request: HelperRequest) throws -> Result {
        let app = try requireControllableApp(request)
        try ensureNoSystemDialogFrontmost()
        // This is the agent's path back to an app after focus shifted away. A
        // bare async activate() loses the race against a just-activated window,
        // so wait for the app to actually become frontmost before reporting
        // success.
        activateAndWait(app)
        // Window-level targeting by CGWindowID is a follow-up (AX windows do not
        // expose CGWindowID); app activation is the robust guarantee.
        // Best-effort: also raise the app's main window.
        let appElement = AXTree.appElement(for: app.processIdentifier)
        if let mainValue = AXTree.copyAttr(appElement, kAXMainWindowAttribute as CFString),
            CFGetTypeID(mainValue) == AXUIElementGetTypeID()
        {
            AXUIElementPerformAction(mainValue as! AXUIElement, kAXRaiseAction as CFString)
        }
        return Result(success: true, usedFallback: false, detail: "focused app")
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

    /// Re-walk the live AX tree to the element named by `request.elementId` (an
    /// index path from the app root) and, if a fingerprint was supplied, verify
    /// the element's identity has not drifted. Throws `stale_element` when the
    /// path no longer resolves or the fingerprint changed.
    private static func resolveElement(app: NSRunningApplication, request: HelperRequest) throws
        -> AXUIElement
    {
        guard let elementId = request.elementId, !elementId.isEmpty else {
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

        if let expected = request.elementFingerprint {
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

    /// Refuse a coordinate target that does not fall inside an on-screen window
    /// owned by the granted app. A raw point is global; without this check a
    /// click or scroll could land on another app's window — including OpenWave's
    /// own consent/Stop/Resume controls — while the broker's audit attributes it
    /// to the granted app. This is the confinement that makes a coordinate
    /// target no broader than an element target. AX frames and CGWindowList
    /// bounds share the global top-left-origin point space, so a point-in-rect
    /// test is valid.
    private static func ensurePointInApp(_ point: CGPoint, app: NSRunningApplication) throws {
        guard
            let infoList = CGWindowListCopyWindowInfo(
                [.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
                as? [[String: Any]]
        else {
            throw HelperError(
                code: .operationFailed, message: "could not enumerate windows to confine a coordinate")
        }
        let pid = app.processIdentifier
        for info in infoList {
            guard
                let ownerPid = (info[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value,
                ownerPid == pid,
                let bounds = info[kCGWindowBounds as String] as? [String: Any]
            else { continue }
            let rect = CGRect(
                x: (bounds["X"] as? NSNumber)?.doubleValue ?? 0,
                y: (bounds["Y"] as? NSNumber)?.doubleValue ?? 0,
                width: (bounds["Width"] as? NSNumber)?.doubleValue ?? 0,
                height: (bounds["Height"] as? NSNumber)?.doubleValue ?? 0)
            if rect.contains(point) { return }
        }
        throw HelperError(
            code: .targetOutsideApp,
            message:
                "the target point is not inside any window owned by \(app.bundleIdentifier ?? "the granted app"); refusing to act on a different app")
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
    /// spin the run loop until the app reports active (bounded) so the whole
    /// burst lands where intended. Fails open: returns after the cap even if
    /// `isActive` never flips (some accessory / full-screen-Space apps report
    /// it oddly) — proceed to type rather than regress the case where
    /// activation actually worked.
    private static func activateAndWait(_ app: NSRunningApplication) {
        app.activate()
        let deadline = Date().addingTimeInterval(activationTimeout)
        while !app.isActive, Date() < deadline {
            // Pump briefly so the workspace's activation notification can land
            // and flip `isActive`, with a sleep floor underneath it: this is a
            // single-shot CLI with no NSApplication, so the run loop usually has
            // no input source and `run(before:)` returns immediately — without
            // the floor the loop would hot-spin a core until activation or the
            // deadline. ~5ms/iteration keeps it cheap.
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
            usleep(5_000)
        }
    }

    // MARK: - CGEvent synthesis

    private static func postMouseClick(at point: CGPoint, button: String, clickCount: Int) throws {
        let (downType, upType, cgButton): (CGEventType, CGEventType, CGMouseButton) =
            button == "right"
            ? (.rightMouseDown, .rightMouseUp, .right)
            : (.leftMouseDown, .leftMouseUp, .left)
        let count = max(1, min(clickCount, 3))
        for click in 1...count {
            guard
                let down = CGEvent(
                    mouseEventSource: nil, mouseType: downType, mouseCursorPosition: point,
                    mouseButton: cgButton),
                let up = CGEvent(
                    mouseEventSource: nil, mouseType: upType, mouseCursorPosition: point,
                    mouseButton: cgButton)
            else {
                throw HelperError(
                    code: .operationFailed, message: "could not synthesize mouse event")
            }
            if count > 1 {
                down.setIntegerValueField(.mouseEventClickState, value: Int64(click))
                up.setIntegerValueField(.mouseEventClickState, value: Int64(click))
            }
            down.post(tap: .cghidEventTap)
            up.post(tap: .cghidEventTap)
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
    private static func typeUnicode(_ text: String) throws {
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
        let source = CGEventSource(stateID: .combinedSessionState)
        // A long burst can run for seconds; a system security dialog appearing
        // mid-burst must stop the remaining keystrokes from landing in it, so
        // re-check the foreground periodically, not only at op start.
        var sinceYieldCheck = 0
        for grapheme in text {
            if sinceYieldCheck >= 64 {
                try ensureNoSystemDialogFrontmost()
                sinceYieldCheck = 0
            }
            var cluster = Array(grapheme.utf16)
            sinceYieldCheck += cluster.count
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
            down.post(tap: .cghidEventTap)
            up.post(tap: .cghidEventTap)
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
