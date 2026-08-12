import AppKit
import ApplicationServices
import Foundation

/// Accessibility-tree read via `AXUIElement`. This is the primary way the
/// agent "sees" an app: it yields on-screen text plus element roles, values,
/// and bounds (reliable click targets), with a far smaller prompt-injection
/// surface than raw pixels. The tree is bounded (depth + node budget +
/// per-string cap) so a large app cannot produce an unbounded result that
/// bloats the model context.
enum AXTree {
    struct Frame: Codable {
        let x: Double
        let y: Double
        let width: Double
        let height: Double
    }

    struct Node: Encodable {
        /// Stable address of this node within the app's tree: a dot-joined path
        /// of child indices from the application root (the root is `"0"`; its
        /// 4th child is `"0.3"`; that child's 2nd is `"0.3.1"`). A control op
        /// re-walks this path in the live tree to retarget the element. An
        /// index path is used because the helper is a fresh process per op — a
        /// memoized `AXUIElement` would be dead by the next call, but an index
        /// path is deterministic and re-resolvable.
        let id: String
        /// Short, cross-process-deterministic hash of the node's identity
        /// (role | title | value-presence | bucketed frame). A control op
        /// recomputes it at action time and refuses with `stale_element` if it
        /// no longer matches, so a click never lands on a different element
        /// that drifted into the same path after a layout change.
        let fingerprint: String
        let role: String?
        let title: String?
        let value: String?
        let frame: Frame?
        let children: [Node]
    }

    struct Result: Encodable {
        let bundleId: String
        let appName: String?
        let tree: Node?
        /// True if the node budget was exhausted before the tree was fully
        /// walked — the agent should know it is seeing a partial view.
        let truncated: Bool
    }

    /// Hard caps the broker's requested bounds are clamped to, so the result
    /// is always bounded regardless of what the caller asks for.
    private static let maxDepthCap = 25
    private static let maxNodesCap = 2000
    private static let maxStringLen = 500

    /// Bound every AX message to the target app. `AXUIElementCopyAttributeValue`
    /// blocks indefinitely against a hung app; without a messaging timeout the
    /// helper never returns a structured error and the broker's stdio loop
    /// (synchronous, one request at a time) wedges for its full 30s kill
    /// window. A timed-out read fails as `operation_failed` instead.
    static func appElement(for pid: pid_t) -> AXUIElement {
        let element = AXUIElementCreateApplication(pid)
        AXUIElementSetMessagingTimeout(element, 5.0)
        return element
    }

    static func read(_ request: HelperRequest) throws -> Result {
        // Surface both permission modals (Accessibility + Screen Recording) up
        // front so the user grants everything in one pass instead of hitting a
        // second modal and restart the first time a different tool runs.
        // read_ax_tree only needs Accessibility, which applies to the live
        // process — a retry after granting succeeds immediately.
        guard Permissions.requestAll().accessibility else {
            throw HelperError(
                code: .permissionDenied, message: "Accessibility permission is not granted")
        }
        guard let bundleId = request.bundleId else {
            throw HelperError(code: .invalidRequest, message: "read_ax_tree requires bundle_id")
        }
        guard
            let app = NSWorkspace.shared.runningApplications.first(where: {
                $0.bundleIdentifier == bundleId
            })
        else {
            throw HelperError(code: .notFound, message: "app \(bundleId) is not running")
        }

        let maxDepth = min(request.maxDepth ?? 12, maxDepthCap)
        let maxNodes = min(request.maxNodes ?? 500, maxNodesCap)

        let appElement = appElement(for: app.processIdentifier)
        var budget = maxNodes
        let tree = buildNode(appElement, depth: 0, maxDepth: maxDepth, path: "0", budget: &budget)
        return Result(
            bundleId: bundleId, appName: app.localizedName, tree: tree, truncated: budget <= 0)
    }

    private static func buildNode(
        _ element: AXUIElement, depth: Int, maxDepth: Int, path: String, budget: inout Int
    ) -> Node? {
        if budget <= 0 { return nil }
        budget -= 1

        let role = copyString(element, kAXRoleAttribute as CFString)
        let title =
            copyString(element, kAXTitleAttribute as CFString)
            ?? copyString(element, kAXDescriptionAttribute as CFString)
        let value = copyValueString(element, kAXValueAttribute as CFString)
        let frame = copyFrame(element)

        var children: [Node] = []
        if depth < maxDepth {
            // Index by raw position in the element's child array — the same
            // indices a control op re-walks to resolve `id`. Children that
            // exceed the budget are dropped from the result, but their absent
            // index still maps to the same live child, so paths stay valid.
            for (index, child) in copyChildren(element).enumerated() {
                if budget <= 0 { break }
                if let node = buildNode(
                    child, depth: depth + 1, maxDepth: maxDepth, path: "\(path).\(index)",
                    budget: &budget)
                {
                    children.append(node)
                }
            }
        }
        return Node(
            id: path,
            fingerprint: fingerprint(role: role, title: title, hasValue: value != nil, frame: frame),
            role: role, title: title, value: value, frame: frame, children: children)
    }

    /// A short, cross-process-deterministic fingerprint of a node's identity.
    /// Uses FNV-1a — deliberately not Swift's `Hasher`, whose seed is
    /// randomized per process: the tree is read in one helper process and
    /// re-validated in a later one, so a per-process seed would make every
    /// element look stale.
    static func fingerprint(role: String?, title: String?, hasValue: Bool, frame: Frame?) -> String {
        let bucket =
            frame.map {
                "\(roundTo5($0.x)),\(roundTo5($0.y)),\(roundTo5($0.width)),\(roundTo5($0.height))"
            } ?? "-"
        let material = "\(role ?? "-")|\(title ?? "-")|\(hasValue ? "1" : "0")|\(bucket)"
        var hash: UInt64 = 0xcbf2_9ce4_8422_2325
        for byte in material.utf8 {
            hash ^= UInt64(byte)
            hash = hash &* 0x0000_0100_0000_01b3
        }
        return String(String(format: "%016llx", hash).prefix(12))
    }

    /// Bucket a coordinate to the nearest 5pt so sub-pixel / minor-layout
    /// jitter does not invalidate a fingerprint (a button nudged 1px is still
    /// the same button).
    private static func roundTo5(_ value: Double) -> Int {
        Int((value / 5.0).rounded()) * 5
    }

    static func copyAttr(_ element: AXUIElement, _ attr: CFString) -> CFTypeRef? {
        var value: CFTypeRef?
        let err = AXUIElementCopyAttributeValue(element, attr, &value)
        return err == .success ? value : nil
    }

    static func copyString(_ element: AXUIElement, _ attr: CFString) -> String? {
        guard let value = copyAttr(element, attr), CFGetTypeID(value) == CFStringGetTypeID() else {
            return nil
        }
        return truncate(value as! String)
    }

    /// `kAXValueAttribute` can be a string, number, or boolean depending on the
    /// element; render each to a short string. Non-scalar values (e.g. an
    /// AXValue struct) are omitted.
    static func copyValueString(_ element: AXUIElement, _ attr: CFString) -> String? {
        guard let value = copyAttr(element, attr) else { return nil }
        let typeID = CFGetTypeID(value)
        if typeID == CFStringGetTypeID() {
            return truncate(value as! String)
        }
        if typeID == CFBooleanGetTypeID() {
            return CFBooleanGetValue((value as! CFBoolean)) ? "true" : "false"
        }
        if typeID == CFNumberGetTypeID() {
            return (value as! NSNumber).stringValue
        }
        return nil
    }

    static func copyChildren(_ element: AXUIElement) -> [AXUIElement] {
        guard let value = copyAttr(element, kAXChildrenAttribute as CFString),
            CFGetTypeID(value) == CFArrayGetTypeID()
        else { return [] }
        return (value as? [AXUIElement]) ?? []
    }

    static func copyFrame(_ element: AXUIElement) -> Frame? {
        guard let posValue = copyAttr(element, kAXPositionAttribute as CFString),
            let sizeValue = copyAttr(element, kAXSizeAttribute as CFString),
            CFGetTypeID(posValue) == AXValueGetTypeID(),
            CFGetTypeID(sizeValue) == AXValueGetTypeID()
        else { return nil }

        var point = CGPoint.zero
        var size = CGSize.zero
        let posOk = AXValueGetValue(posValue as! AXValue, .cgPoint, &point)
        let sizeOk = AXValueGetValue(sizeValue as! AXValue, .cgSize, &size)
        guard posOk, sizeOk else { return nil }
        return Frame(x: point.x, y: point.y, width: size.width, height: size.height)
    }

    private static func truncate(_ string: String) -> String {
        string.count > maxStringLen ? String(string.prefix(maxStringLen)) : string
    }
}
