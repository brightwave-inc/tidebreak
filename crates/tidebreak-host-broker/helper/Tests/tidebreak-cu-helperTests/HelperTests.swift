import CoreGraphics
import Foundation

@main
struct HelperTests {
    static func main() throws {
        let suite = HelperTests()
        try suite.testTextFieldDecodesForTypingAndWaits()
        print("PASS testTextFieldDecodesForTypingAndWaits")
        suite.testDragAlwaysReleasesAfterMoveFailure()
        print("PASS testDragAlwaysReleasesAfterMoveFailure")
        suite.testSuccessfulDragReleasesExactlyOnce()
        print("PASS testSuccessfulDragReleasesExactlyOnce")
        suite.testDragPointsBoundStepsAndReachExactEndpoint()
        print("PASS testDragPointsBoundStepsAndReachExactEndpoint")
        try suite.testCancellationReleasesDragAndRefusesOldGeneration()
        print("PASS testCancellationReleasesDragAndRefusesOldGeneration")
        suite.testSearchDoesNotReportAbsenceWhenTraversalIsIncomplete()
        print("PASS testSearchDoesNotReportAbsenceWhenTraversalIsIncomplete")
        suite.testCaptureCoordinatesAccountForCropOriginAndPixelScale()
        print("PASS testCaptureCoordinatesAccountForCropOriginAndPixelScale")
        try suite.testScreenshotResponseIncludesCoordinateFrame()
        print("PASS testScreenshotResponseIncludesCoordinateFrame")
        try suite.testDownscaleReturnsActualDimensionsWithoutEnlargingSmallImages()
        print("PASS testDownscaleReturnsActualDimensionsWithoutEnlargingSmallImages")
        suite.testResizeVerificationRejectsClampedDimension()
        print("PASS testResizeVerificationRejectsClampedDimension")
        suite.testPointerTargetRejectsAnotherWindowCoveringTheGrantedApp()
        print("PASS testPointerTargetRejectsAnotherWindowCoveringTheGrantedApp")
        try suite.testAXIdentifierIsAvailableForFixtureTargets()
        print("PASS testAXIdentifierIsAvailableForFixtureTargets")
        suite.testScrollDirectionMatchesAPI()
        print("PASS testScrollDirectionMatchesAPI")
        suite.testDisplayCaptureKeepsGlobalPlacement()
        print("PASS testDisplayCaptureKeepsGlobalPlacement")
        print("14 helper regression tests passed")
    }

    func testScrollDirectionMatchesAPI() {
        expectEqual(Control.scrollWheelDelta(180), -180)
        expectEqual(Control.scrollWheelDelta(-180), 180)
        expectEqual(Control.scrollWheelDelta(1e20), Int32.min)
        expectEqual(Control.scrollWheelDelta(-1e20), Int32.max)
    }

    func testDisplayCaptureKeepsGlobalPlacement() {
        let frame = CGRect(x: -1920, y: 100, width: 1920, height: 1080)
        let display = Capture.configuration(
            width: 1440, height: 810, coordinateFrame: frame, displayScoped: true)
        expectEqual(display.sourceRect, CGRect(x: 0, y: 0, width: 1920, height: 1080))
        expectEqual(display.destinationRect, CGRect(x: 0, y: 0, width: 1440, height: 810))
        expectFalse(display.preservesAspectRatio)
        let window = Capture.configuration(
            width: 500, height: 400, coordinateFrame: frame, displayScoped: false)
        expectEqual(window.sourceRect, .zero)
        expectTrue(window.ignoreShadowsSingleWindow)
    }

    private func request(_ json: String) throws -> HelperRequest {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(HelperRequest.self, from: Data(json.utf8))
    }

    func testTextFieldDecodesForTypingAndWaits() throws {
        expectEqual(try request(#"{"op":"type_text","text":"hello"}"#).text, "hello")
        let wait = try request(
            #"{"op":"wait_condition","condition":"text_absent","text":"pending"}"#)
        expectEqual(wait.text, "pending")
        expectEqual(wait.condition, .textAbsent)
    }

    func testDragAlwaysReleasesAfterMoveFailure() {
        enum Failure: Error { case moved }
        var events: [String] = []
        expectError(
            try Control.deliverDrag(
                count: 4,
                press: { events.append("press") },
                move: { index in
                    events.append("move\(index)")
                    if index == 1 { throw Failure.moved }
                },
                release: { events.append("release") }, pause: { events.append("pause") }))
        expectEqual(events, ["press", "pause", "move0", "pause", "move1", "release"])
    }

    func testSuccessfulDragReleasesExactlyOnce() {
        var events: [String] = []
        Control.deliverDrag(
            count: 2, press: { events.append("press") },
            move: { events.append("move\($0)") }, release: { events.append("release") },
            pause: { events.append("pause") })
        expectEqual(events, ["press", "pause", "move0", "pause", "move1", "release"])
    }

    func testDragPointsBoundStepsAndReachExactEndpoint() {
        let from = CGPoint(x: -100, y: 40)
        let to = CGPoint(x: 500, y: 340)
        for duration in [-1, 0, 200, 10_000, Int.max] {
            let points = Control.dragPoints(from: from, to: to, durationMs: duration)
            expectAtLeast(points.count, 20)
            expectAtMost(points.count, 1000)
            expectEqual(points.last, to)
            expectTrue(points.allSatisfy { $0.x > from.x && $0.x <= to.x })
        }
        expectEqual(Control.dragPoints(from: from, to: to, durationMs: 200).count, 40)
    }

    func testCancellationReleasesDragAndRefusesOldGeneration() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let file = directory.appendingPathComponent("generation")
        try Data("active".utf8).write(to: file)
        let payload: [String: Any] = [
            "op": "drag", "cancel_path": file.path, "cancel_generation": "active",
        ]
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let command = try decoder.decode(
            HelperRequest.self, from: JSONSerialization.data(withJSONObject: payload))
        expectSuccess(try Control.ensureNotCancelled(command))
        var moves = 0
        var releases = 0
        expectError(
            try Control.deliverDrag(
                count: 3, press: {},
                move: { _ in
                    try Control.ensureNotCancelled(command)
                    moves += 1
                    try Data("stopped".utf8).write(to: file, options: .atomic)
                }, release: { releases += 1 }, pause: {})
        ) { error in
            expectEqual((error as? HelperError)?.code, .yielded)
        }
        expectEqual(moves, 1)
        expectEqual(releases, 1)
        try Data("resumed".utf8).write(to: file, options: .atomic)
        expectError(try Control.ensureNotCancelled(command))
        try FileManager.default.removeItem(at: file)
        expectError(try Control.ensureNotCancelled(command))
        expectSuccess(try Control.ensureNotCancelled(try request(#"{"op":"click"}"#)))
    }

    func testSearchDoesNotReportAbsenceWhenTraversalIsIncomplete() {
        let nodes = [
            Control.TextNode(strings: ["root"], children: [1, 2], complete: true),
            Control.TextNode(strings: ["first"], children: [], complete: true),
            Control.TextNode(strings: ["needle"], children: [], complete: true),
        ]
        func search(
            _ text: String, maxNodes: Int = 20, maxDepth: Int = 25,
            deadline: Double = 1
        ) -> Control.TextObservation {
            Control.searchText(
                root: 0, text: text, maxNodes: maxNodes, maxDepth: maxDepth,
                deadline: deadline, now: { 0 }, read: { nodes[$0] })
        }
        expectEqual(search("needle"), .present)
        expectEqual(search("missing"), .absent)
        expectEqual(search("needle", maxNodes: 2), .incomplete)
        expectEqual(search("needle", maxDepth: 0), .incomplete)
        expectEqual(search("needle", deadline: 0), .incomplete)
        expectFalse(Control.TextObservation.incomplete.satisfies(.textAbsent))
        expectFalse(Control.TextObservation.incomplete.satisfies(.textPresent))
        expectTrue(Control.TextObservation.absent.satisfies(.textAbsent))
        let unreadable = Control.searchText(
            root: 0, text: "missing", deadline: 1, now: { 0 },
            read: { _ in Control.TextNode<Int>(strings: [], children: [], complete: false) })
        expectEqual(unreadable, .incomplete)
    }

    func testCaptureCoordinatesAccountForCropOriginAndPixelScale() {
        let crop = CGRect(x: -1920, y: 200, width: 960, height: 600)
        let target = CGRect(x: -1820, y: 250, width: 80, height: 40)
        let pixels = Capture.pixelRect(for: target, coordinateFrame: crop, width: 480, height: 300)
        expectEqual(pixels, CGRect(x: 50, y: 25, width: 40, height: 20))
    }

    func testScreenshotResponseIncludesCoordinateFrame() throws {
        let result = Capture.Result(
            width: 600, height: 400, path: "/tmp/test.png", mediaType: "image/png",
            coordinateFrame: .init(CGRect(x: -400, y: 50, width: 1200, height: 800)))
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        let json = try requireValue(
            JSONSerialization.jsonObject(with: encoder.encode(result)) as? [String: Any])
        let frame = try requireValue(json["coordinate_frame"] as? [String: Double])
        expectEqual(frame, ["x": -400, "y": 50, "width": 1200, "height": 800])
        expectEqual(json["width"] as? Int, 600)
    }

    private func image(width: Int, height: Int) throws -> CGImage {
        let context = try requireValue(
            CGContext(
                data: nil, width: width, height: height,
                bitsPerComponent: 8, bytesPerRow: 0, space: CGColorSpaceCreateDeviceRGB(),
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
        return try requireValue(context.makeImage())
    }

    func testDownscaleReturnsActualDimensionsWithoutEnlargingSmallImages() throws {
        let small = try image(width: 20, height: 10)
        expectNil(try Capture.downscaleForBudget(small, maxDimension: 100))
        let wide = try image(width: 1000, height: 100)
        let scaled = try requireValue(Capture.downscaleForBudget(wide, maxDimension: 200))
        expectEqual(scaled.width, 200)
        expectEqual(scaled.height, 20)
        let tiny = try requireValue(Capture.downscaleForBudget(wide, maxDimension: 1))
        expectEqual(tiny.width, 1)
        expectEqual(tiny.height, 1)
    }

    func testResizeVerificationRejectsClampedDimension() {
        expectTrue(
            Control.sizeMatches(
                frame: .init(x: 0, y: 0, width: 800, height: 600),
                requested: CGSize(width: 800, height: 600)))
        expectFalse(
            Control.sizeMatches(
                frame: .init(x: 0, y: 0, width: 800, height: 550),
                requested: CGSize(width: 800, height: 600)))
    }

    func testPointerTargetRejectsAnotherWindowCoveringTheGrantedApp() {
        let granted = Control.AppWindow(
            windowId: 1, ownerPid: 100,
            frame: CGRect(x: 0, y: 0, width: 800, height: 600))
        let overlay = Control.AppWindow(
            windowId: 2, ownerPid: 200,
            frame: CGRect(x: 100, y: 100, width: 100, height: 100))
        expectTrue(
            Control.pointBelongsToApp(CGPoint(x: 50, y: 50), pid: 100, windows: [overlay, granted]))
        expectFalse(
            Control.pointBelongsToApp(
                CGPoint(x: 150, y: 150), pid: 100, windows: [overlay, granted]))
        expectFalse(
            Control.pointBelongsToApp(
                CGPoint(x: 900, y: 50), pid: 100, windows: [overlay, granted]))
    }

    func testAXIdentifierIsAvailableForFixtureTargets() throws {
        let node = AXTree.Node(
            id: "0.1", fingerprint: "abc", role: "AXButton", identifier: "test.submit",
            title: "Submit", value: nil, frame: nil, children: [])
        let object = try requireValue(
            JSONSerialization.jsonObject(with: JSONEncoder().encode(node)) as? [String: Any])
        expectEqual(object["identifier"] as? String, "test.submit")
    }
}

private func expectEqual<T: Equatable>(
    _ actual: T, _ expected: T, file: StaticString = #file, line: UInt = #line
) {
    precondition(actual == expected, "Expected \(expected), got \(actual)", file: file, line: line)
}
private func expectTrue(_ value: Bool, file: StaticString = #file, line: UInt = #line) {
    precondition(value, "Expected true", file: file, line: line)
}
private func expectFalse(_ value: Bool, file: StaticString = #file, line: UInt = #line) {
    precondition(!value, "Expected false", file: file, line: line)
}
private func expectNil<T>(_ value: T?, file: StaticString = #file, line: UInt = #line) {
    precondition(value == nil, "Expected nil", file: file, line: line)
}
private func expectAtLeast<T: Comparable>(_ actual: T, _ expected: T) {
    precondition(actual >= expected)
}
private func expectAtMost<T: Comparable>(_ actual: T, _ expected: T) {
    precondition(actual <= expected)
}
private func requireValue<T>(_ value: T?) throws -> T {
    guard let value else { throw TestFailure.expectedValue }
    return value
}
private enum TestFailure: Error { case expectedValue }
private func expectError<T>(
    _ work: @autoclosure () throws -> T, check: (Error) -> Void = { _ in },
    file: StaticString = #file, line: UInt = #line
) {
    do {
        _ = try work()
        preconditionFailure("Expected an error", file: file, line: line)
    } catch { check(error) }
}
private func expectSuccess<T>(
    _ work: @autoclosure () throws -> T, file: StaticString = #file, line: UInt = #line
) {
    do { _ = try work() } catch {
        preconditionFailure("Unexpected error: \(error)", file: file, line: line)
    }
}
