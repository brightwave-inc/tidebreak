import ApplicationServices
import CoreGraphics
import Foundation

/// macOS TCC permission status for the two grants computer use needs, plus the
/// prompting path that requests them. `status()` is pure preflight and never
/// prompts; `requestAll()` actively triggers the system modals.
enum Permissions {
    struct Status: Encodable {
        /// Screen Recording (required for `capture`).
        /// `CGPreflightScreenCaptureAccess` checks without prompting.
        let screenRecording: Bool
        /// Accessibility (required for `read_ax_tree` and for input
        /// synthesis). `AXIsProcessTrusted` checks without prompting.
        let accessibility: Bool
    }

    static func status() -> Status {
        Status(
            screenRecording: CGPreflightScreenCaptureAccess(),
            accessibility: AXIsProcessTrusted()
        )
    }

    /// Actively request both TCC grants computer use needs, so macOS surfaces
    /// the Screen Recording and Accessibility modals together rather than one
    /// tool prompting for its own grant now and a different tool prompting for
    /// the other (plus a second restart) later. Requesting a grant already
    /// held is a no-op (no modal). Screen Recording activates only on a fresh
    /// process; Accessibility applies to the live process. Returns the
    /// post-request status.
    @discardableResult
    static func requestAll() -> Status {
        if !CGPreflightScreenCaptureAccess() {
            _ = CGRequestScreenCaptureAccess()
        }
        if !AXIsProcessTrusted() {
            let options =
                [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary
            _ = AXIsProcessTrustedWithOptions(options)
        }
        return status()
    }
}
