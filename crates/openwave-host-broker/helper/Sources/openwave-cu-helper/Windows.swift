import AppKit
import CoreGraphics
import Foundation

/// On-screen window enumeration. Lets the agent discover what windows an app
/// has open (and their bounds) before capturing or reading one. Window titles
/// require Screen Recording on recent macOS; without it the title comes back
/// nil but ids, bounds, and the owning app still resolve.
enum Windows {
    struct Frame: Encodable {
        let x: Double
        let y: Double
        let width: Double
        let height: Double
    }

    struct Window: Encodable {
        let windowId: UInt32
        let title: String?
        let appName: String?
        let bundleId: String?
        let pid: Int32
        let frame: Frame
    }

    static func list(bundleId: String?) throws -> [Window] {
        try Control.ensureNotBlocked(bundleId)
        // Enumeration itself works without a grant (titles just come back
        // nil), but this is usually the first computer-use op the agent runs,
        // so surface both permission modals here. That way the user grants
        // Screen Recording and Accessibility together at the first step and
        // the later capture/read calls find them already granted instead of
        // prompting one at a time.
        Permissions.requestAll()

        let running = NSWorkspace.shared.runningApplications
        // pid → bundle id, so every listed window can name its owning app.
        var bundleByPid: [pid_t: String] = [:]
        for app in running {
            if let bid = app.bundleIdentifier {
                bundleByPid[app.processIdentifier] = bid
            }
        }
        // Optional filter: the pids belonging to the requested bundle id (an
        // app can have several).
        let targetPids: Set<pid_t>? = bundleId.map { bid in
            Set(running.filter { $0.bundleIdentifier == bid }.map { $0.processIdentifier })
        }

        guard
            let infoList = CGWindowListCopyWindowInfo(
                [.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
                as? [[String: Any]]
        else {
            throw HelperError(code: .operationFailed, message: "could not enumerate windows")
        }

        var windows: [Window] = []
        for info in infoList {
            guard
                let wid = (info[kCGWindowNumber as String] as? NSNumber)?.uint32Value,
                let pid = (info[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value
            else { continue }
            if let targets = targetPids, !targets.contains(pid) { continue }

            let bounds = info[kCGWindowBounds as String] as? [String: Any]
            let frame = Frame(
                x: (bounds?["X"] as? NSNumber)?.doubleValue ?? 0,
                y: (bounds?["Y"] as? NSNumber)?.doubleValue ?? 0,
                width: (bounds?["Width"] as? NSNumber)?.doubleValue ?? 0,
                height: (bounds?["Height"] as? NSNumber)?.doubleValue ?? 0)

            windows.append(
                Window(
                    windowId: wid,
                    title: info[kCGWindowName as String] as? String,
                    appName: info[kCGWindowOwnerName as String] as? String,
                    bundleId: bundleByPid[pid],
                    pid: pid,
                    frame: frame))
        }
        return windows
    }
}
