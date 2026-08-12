import Foundation

// Entry point and wire protocol for the computer-use helper.
//
// Invocation model: the broker spawns the helper once per operation, writes a
// single JSON request object to stdin, and reads a single JSON response object
// from stdout. The process then exits. Single-shot (vs a long-running sidecar)
// keeps the helper stateless and makes the macOS TCC attribution per-invocation
// simple; the broker already owns the persistent state and policy.
//
// This helper-internal protocol is distinct from the broker↔host stdio
// protocol. The broker translates between them, so the agent never sees these
// shapes and they can evolve independently.

/// The operation the broker is asking the helper to perform. snake_case on the
/// wire.
enum HelperOp: String, Decodable {
    case permissions
    case requestPermissions = "request_permissions"
    case listWindows = "list_windows"
    case capture
    case readAxTree = "read_ax_tree"
    // Control ops (Accessibility input synthesis). `wait` is intentionally
    // absent — it is an inert broker-side sleep that never reaches the helper.
    case click
    case typeText = "type_text"
    case keyPress = "key_press"
    case scroll
    case focusWindow = "focus_window"
    // Read-only: report a target element's role + label without acting, for the
    // broker's forced-confirmation tripwire (it classifies whether a control op
    // is consequential before acting).
    case describeElement = "describe_element"
}

/// What a `capture` request targets.
enum CaptureTargetKind: String, Decodable {
    case display
    case window
    case app
}

/// A single helper request. All operation parameters are optional and validated
/// per-op; an absent required field yields a structured `invalid_request` error
/// rather than a crash.
struct HelperRequest: Decodable {
    let op: HelperOp
    /// `capture` target discriminator.
    let target: CaptureTargetKind?
    /// macOS bundle id (e.g. "com.apple.Notes") — for app-scoped capture and
    /// read_ax_tree, and the optional list_windows filter.
    let bundleId: String?
    /// CGWindowID for window-scoped capture.
    let windowId: UInt32?
    /// CGDirectDisplayID for display-scoped capture. Absent → the main display.
    let displayId: UInt32?
    /// Absolute path the helper writes the captured PNG to. The broker creates
    /// this under its own owner-only staging dir and passes it in; the helper
    /// never chooses the destination.
    let outPath: String?
    /// capture: optional numbered targets to draw over the screenshot for
    /// Set-of-Marks visual grounding.
    let marks: [CaptureMark]?
    /// read_ax_tree bounds. The broker supplies defaults; the helper clamps to
    /// hard caps.
    let maxDepth: Int?
    let maxNodes: Int?
    /// Control ops: the AX element to act on, addressed by its `id`
    /// (index-path) from a prior read_ax_tree. Absent → the op uses the `x`/`y`
    /// coordinate fallback (or targets the focused app, for key_press).
    let elementId: String?
    /// Control ops: the element's `fingerprint` from the read, re-checked at
    /// action time to detect drift.
    let elementFingerprint: String?
    /// Control ops: coordinate fallback (global, top-left origin — same space
    /// as AX frames) when no element.
    let x: Double?
    let y: Double?
    /// type_text: the text to enter.
    let text: String?
    /// key_press: the key name (e.g. "return", "a", "left") and its chord
    /// modifiers (cmd/shift/ctrl/alt).
    let key: String?
    let modifiers: [String]?
    /// click: "left" (default) or "right", and the click count (1 = single, 2 =
    /// double).
    let button: String?
    let clickCount: Int?
    /// scroll: pixel deltas (positive dy scrolls down, positive dx scrolls
    /// right).
    let dx: Double?
    let dy: Double?
}

struct CaptureMark: Decodable {
    let mark: Int
    let frame: AXTree.Frame
}

/// Structured failure codes, so the broker can map them to a retryable flag /
/// consent prompt rather than pattern-matching on message strings.
enum HelperErrorCode: String, Encodable {
    /// The request JSON was missing a required field or otherwise malformed.
    case invalidRequest = "invalid_request"
    /// A required macOS TCC permission (Screen Recording / Accessibility) is
    /// not granted.
    case permissionDenied = "permission_denied"
    /// The named app/window/display could not be found (e.g. the app is not
    /// running).
    case notFound = "not_found"
    /// A control op's target element no longer exists at its addressed path, or
    /// its fingerprint changed since it was read — the UI shifted. The agent
    /// should re-read the app content and retry.
    case staleElement = "stale_element"
    /// A safety guard backed off instead of acting: a system
    /// security/authorization dialog owns the foreground (synthesizing input
    /// could drive it). The agent should not retry — the situation is the
    /// user's to resolve.
    case yielded = "yielded"
    /// A coordinate target falls outside every on-screen window the granted
    /// app owns — acting there would drive a different app (or OpenWave's own
    /// consent surface). Refused, not acted on.
    case targetOutsideApp = "target_outside_app"
    /// The native API failed for some other reason.
    case operationFailed = "operation_failed"
}

struct HelperError: Error {
    let code: HelperErrorCode
    let message: String
}

@main
struct CUHelper {
    static func main() async {
        let request: HelperRequest
        do {
            let input = FileHandle.standardInput.readDataToEndOfFile()
            let decoder = JSONDecoder()
            decoder.keyDecodingStrategy = .convertFromSnakeCase
            request = try decoder.decode(HelperRequest.self, from: input)
        } catch {
            emitError(HelperError(code: .invalidRequest, message: "could not parse request: \(error)"))
            return
        }

        do {
            switch request.op {
            case .permissions:
                emit(Permissions.status())
            case .requestPermissions:
                emit(Permissions.requestAll())
            case .listWindows:
                emit(try Windows.list(bundleId: request.bundleId))
            case .capture:
                emit(try await Capture.run(request))
            case .readAxTree:
                emit(try AXTree.read(request))
            case .click:
                emit(try Control.click(request))
            case .typeText:
                emit(try Control.typeText(request))
            case .keyPress:
                emit(try Control.keyPress(request))
            case .scroll:
                emit(try Control.scroll(request))
            case .focusWindow:
                emit(try Control.focusWindow(request))
            case .describeElement:
                emit(try Control.describeElement(request))
            }
        } catch let error as HelperError {
            emitError(error)
        } catch {
            emitError(HelperError(code: .operationFailed, message: "\(error)"))
        }
    }
}

// MARK: - Output

/// Wraps a successful result as `{"ok":true,"result":<...>}`.
private struct OkEnvelope<T: Encodable>: Encodable {
    let ok: Bool
    let result: T
}

/// `{"ok":false,"code":"...","error":"..."}`.
private struct ErrEnvelope: Encodable {
    let ok: Bool
    let code: HelperErrorCode
    let error: String
}

private func encoder() -> JSONEncoder {
    let encoder = JSONEncoder()
    encoder.keyEncodingStrategy = .convertToSnakeCase
    return encoder
}

func emit<T: Encodable>(_ result: T) {
    let envelope = OkEnvelope(ok: true, result: result)
    write(envelope)
}

func emitError(_ error: HelperError) {
    write(ErrEnvelope(ok: false, code: error.code, error: error.message))
}

private func write<T: Encodable>(_ value: T) {
    guard let data = try? encoder().encode(value) else {
        FileHandle.standardOutput.write(
            Data(#"{"ok":false,"code":"operation_failed","error":"could not encode response"}"#.utf8))
        return
    }
    FileHandle.standardOutput.write(data)
}
