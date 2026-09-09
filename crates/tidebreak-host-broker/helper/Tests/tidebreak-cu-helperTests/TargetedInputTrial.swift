import AppKit
import CoreGraphics
import Foundation

/// Compile this entry point explicitly. It is absent from the packaged helper.
@main
struct TargetedInputTrial {
    struct Request: Decodable {
        let pid: Int32
        let windowId: UInt32
        let action: String
        let x: Double?
        let y: Double?
        let toX: Double?
        let toY: Double?
        let key: String?
        let text: String?
        let dx: Double?
        let dy: Double?
        let cancelPath: String
        let cancelValue: String
    }

    static func output(_ value: [String: Any]) {
        if let data = try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]) {
            FileHandle.standardOutput.write(data)
            FileHandle.standardOutput.write(Data([10]))
        }
    }

    static func outputObservation(_ phase: String, _ observed: TargetedInput.Observation) {
        let target = observed.target
        output([
            "phase": phase, "target_pid": target.pid, "launch_time": target.launchedAt,
            "bundle_id": target.bundleId, "window_id": target.windowId,
            "frame": [
                "x": target.frame.minX, "y": target.frame.minY,
                "width": target.frame.width, "height": target.frame.height,
            ],
            "frontmost_pid": observed.frontmostPid ?? -1,
            "pointer": ["x": observed.pointer.x, "y": observed.pointer.y],
        ])
    }

    @MainActor
    static func main() {
        NSApplication.shared.setActivationPolicy(.prohibited)
        do {
            let live = CommandLine.arguments.dropFirst().elementsEqual(["--deliver"])
            guard CommandLine.arguments.count == 1 || live else {
                throw TargetedInput.Failure(
                    reason: "use --deliver only for a supervised trial", delivered: 0, released: 0)
            }
            let decoder = JSONDecoder()
            decoder.keyDecodingStrategy = .convertFromSnakeCase
            guard let bytes = try FileHandle.standardInput.read(upToCount: 16 * 1024 + 1),
                bytes.count <= 16 * 1024
            else {
                throw TargetedInput.Failure(reason: "oversized request", delivered: 0, released: 0)
            }
            let request = try decoder.decode(Request.self, from: bytes)
            guard !live || AXIsProcessTrusted(), request.pid > 0, request.windowId > 0,
                request.cancelPath.hasPrefix("/"), UUID(uuidString: request.cancelValue) != nil,
                let app = NSRunningApplication(processIdentifier: request.pid),
                app.bundleIdentifier == "dev.tidebreak.ComputerUseFixture", !app.isTerminated,
                let launchedAt = app.launchDate?.timeIntervalSince1970
            else {
                throw TargetedInput.Failure(
                    reason: "requires existing Accessibility and the fixture process", delivered: 0,
                    released: 0)
            }
            let seed = TargetedInput.Target(
                pid: request.pid, bundleId: "dev.tidebreak.ComputerUseFixture",
                launchedAt: launchedAt, windowId: request.windowId, frame: .zero)
            let target = try TargetedInput.observe(seed).target
            outputObservation("before", try TargetedInput.observe(target))
            defer {
                if let after = try? TargetedInput.observe(target) {
                    outputObservation("after", after)
                } else {
                    output(["phase": "after", "observation_error": "target unavailable"])
                }
            }
            let session = try TargetedInput.Session(
                target: target,
                observation: { try TargetedInput.observe(target) },
                checkCancellation: {
                    guard let data = FileManager.default.contents(atPath: request.cancelPath),
                        data.count <= 128,
                        String(data: data, encoding: .utf8) == request.cancelValue
                    else {
                        throw TargetedInput.Failure(
                            reason: "trial cancelled", delivered: 0, released: 0)
                    }
                },
                deliver: { event, pid in
                    output([
                        "phase": "dispatch", "pid": pid, "event_type": event.type.rawValue,
                        "target_pid": event.getIntegerValueField(.eventTargetUnixProcessID),
                        "window": event.getIntegerValueField(.mouseEventWindowUnderMousePointer),
                        "appkit_window": NSEvent(cgEvent: event)?.windowNumber ?? -1,
                        "window_local_x": EventWindowLocation.get(event)?.x ?? -9999,
                        "window_local_y": EventWindowLocation.get(event)?.y ?? -9999,
                        "source_state": CGEventSource(event: event)?.sourceStateID.rawValue ?? -999,
                        "x": event.location.x, "y": event.location.y,
                    ])
                    event.postToPid(pid)
                },
                pause: { interval in
                    RunLoop.current.run(until: Date().addingTimeInterval(interval))
                }, requireUnchangedDesktop: false)
            let steps: [TargetedInput.Step]
            if request.action == "text", let text = request.text {
                steps = try session.textSteps(text, maxUnits: 50)
            } else if request.action == "key" {
                let code: CGKeyCode
                let characters: String
                switch request.key {
                case "a":
                    code = 0
                    characters = "a"
                case "return":
                    code = 36
                    characters = "\r"
                case "escape":
                    code = 53
                    characters = "\u{1B}"
                default:
                    throw TargetedInput.Failure(
                        reason: "trial key must be a, return, or escape", delivered: 0, released: 0)
                }
                let down = try session.key(code, down: true, characters: characters)
                let up = try session.key(code, down: false, characters: characters)
                steps = [.init(event: down, release: up, delay: 0.02)]
            } else {
                guard let x = request.x, let y = request.y else {
                    throw TargetedInput.Failure(
                        reason: "trial pointer coordinates required", delivered: 0, released: 0)
                }
                let from = CGPoint(x: x, y: y)
                if request.action == "hover" {
                    steps = [
                        .init(
                            event: try session.mouse(.mouseMoved, at: from), release: nil,
                            delay: 0.05)
                    ]
                } else if request.action == "scroll" {
                    guard let dx = request.dx, let dy = request.dy,
                        abs(dx) <= 500, abs(dy) <= 500
                    else {
                        throw TargetedInput.Failure(
                            reason: "trial scroll requires bounded dx/dy", delivered: 0, released: 0
                        )
                    }
                    steps = [
                        .init(
                            event: try session.scroll(at: from, dx: dx, dy: dy), release: nil,
                            delay: 0.05)
                    ]
                } else if request.action == "double_click" {
                    let firstDown = try session.mouse(.leftMouseDown, at: from, clickCount: 1)
                    let firstUp = try session.mouse(.leftMouseUp, at: from, clickCount: 1)
                    let secondDown = try session.mouse(.leftMouseDown, at: from, clickCount: 2)
                    let secondUp = try session.mouse(.leftMouseUp, at: from, clickCount: 2)
                    steps = [
                        .init(event: firstDown, release: firstUp, delay: 0.02),
                        .init(event: firstUp, release: nil, delay: 0.05),
                        .init(event: secondDown, release: secondUp, delay: 0.02),
                    ]
                } else if request.action == "click" || request.action == "right_click" {
                    let down = try session.mouse(
                        request.action == "right_click" ? .rightMouseDown : .leftMouseDown, at: from
                    )
                    let up = try session.mouse(
                        request.action == "right_click" ? .rightMouseUp : .leftMouseUp, at: from)
                    down.setIntegerValueField(.mouseEventNumber, value: 1)
                    up.setIntegerValueField(.mouseEventNumber, value: 1)
                    steps = [.init(event: down, release: up, delay: 0.02)]
                } else if request.action == "drag", let x = request.toX, let y = request.toY {
                    let to = CGPoint(x: x, y: y)
                    let down = try session.mouse(.leftMouseDown, at: from)
                    let up = try session.mouse(.leftMouseUp, at: from)
                    down.setIntegerValueField(.mouseEventNumber, value: 1)
                    up.setIntegerValueField(.mouseEventNumber, value: 1)
                    var plan = [TargetedInput.Step(event: down, release: up, delay: 0.01)]
                    for index in 1...20 {
                        let fraction = Double(index) / 20
                        let point = CGPoint(
                            x: from.x + (to.x - from.x) * fraction,
                            y: from.y + (to.y - from.y) * fraction)
                        let move = try session.mouse(.leftMouseDragged, at: point)
                        move.setIntegerValueField(.mouseEventNumber, value: 1)
                        plan.append(.init(event: move, release: nil, delay: 0.01))
                    }
                    steps = plan
                } else {
                    throw TargetedInput.Failure(
                        reason:
                            "trial action must be text, key, hover, click, right_click, double_click, scroll, or drag",
                        delivered: 0, released: 0)
                }
            }
            output([
                "phase": "bound", "pid": target.pid, "launch_time": target.launchedAt,
                "window_id": target.windowId, "action": request.action,
                "mouse_constructor": "window_local_spi",
                "source_state": session.source.sourceStateID.rawValue,
            ])
            guard live else {
                output(["phase": "finished", "status": "dry_run", "events": steps.count])
                return
            }
            let result = try session.run(steps)
            output([
                "phase": "finished", "status": "sent_unverified",
                "delivered": result.delivered, "released": result.released,
            ])
        } catch {
            output(["phase": "failed", "error": String(describing: error)])
            exit(1)
        }
    }
}
