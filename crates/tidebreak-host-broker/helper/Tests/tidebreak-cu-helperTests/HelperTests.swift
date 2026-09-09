import AppKit
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
        suite.testPartialDragReportsUncertainOutcome()
        print("PASS testPartialDragReportsUncertainOutcome")
        try suite.testActivationWaitsForTheFrontmostApp()
        print("PASS testActivationWaitsForTheFrontmostApp")
        suite.testActivationWaitIsBoundedAndCancellable()
        print("PASS testActivationWaitIsBoundedAndCancellable")
        try suite.testSuccessfulDragReleasesExactlyOnce()
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
        try suite.testBackgroundIsDefaultAndForegroundOnlyInputRefusesEarly()
        print("PASS testBackgroundIsDefaultAndForegroundOnlyInputRefusesEarly")
        try suite.testExecutionModeIsExplicitInControlResults()
        print("PASS testExecutionModeIsExplicitInControlResults")
        suite.testBackgroundScrollClampsAtEachEnd()
        print("PASS testBackgroundScrollClampsAtEachEnd")
        try suite.testInputJournalSurvivesUntilRelease()
        print("PASS testInputJournalSurvivesUntilRelease")
        try suite.testRecoveryPreservesPhysicalHoldsAndReleasesInReverseOrder()
        print("PASS testRecoveryPreservesPhysicalHoldsAndReleasesInReverseOrder")
        try suite.testRecoveryRejectsWrongIdentityOversizedAndPublicJournals()
        print("PASS testRecoveryRejectsWrongIdentityOversizedAndPublicJournals")
        try suite.testUpStillReleasesAfterJournalDamage()
        print("PASS testUpStillReleasesAfterJournalDamage")
        try suite.testInvocationCancellationRefusesFurtherInput()
        print("PASS testInvocationCancellationRefusesFurtherInput")
        try suite.testModifierAndMouseEventsHaveTrackedReleases()
        print("PASS testModifierAndMouseEventsHaveTrackedReleases")
        try suite.testTargetedInputUsesPrivateStateAndBoundWindow()
        print("PASS testTargetedInputUsesPrivateStateAndBoundWindow")
        try suite.testTargetedInputStopsOnFocusAndPointerChangesAndReleases()
        print("PASS testTargetedInputStopsOnFocusAndPointerChangesAndReleases")
        try suite.testTargetedInputRejectsPIDReuseAndCancellation()
        print("PASS testTargetedInputRejectsPIDReuseAndCancellation")
        try suite.testTargetedJournalRetainsDestinationAndReleasePayload()
        print("PASS testTargetedJournalRetainsDestinationAndReleasePayload")
        try suite.testProductionTargetAllowsUnrelatedPointerAndAppChanges()
        print("PASS testProductionTargetAllowsUnrelatedPointerAndAppChanges")
        try suite.testTargetedKeysPreserveModifierTranslation()
        print("PASS testTargetedKeysPreserveModifierTranslation")
        try suite.testTargetedMouseRecoveryUsesLastAgentPosition()
        print("PASS testTargetedMouseRecoveryUsesLastAgentPosition")
        try suite.testTargetedScrollPreservesDeltasAndWindow()
        print("PASS testTargetedScrollPreservesDeltasAndWindow")
        try suite.testProductionDragCancellationReleasesWithoutTakeover()
        print("PASS testProductionDragCancellationReleasesWithoutTakeover")
        try suite.testProductionRejectsBeforeDownWithoutRelease()
        print("PASS testProductionRejectsBeforeDownWithoutRelease")
        try suite.testTargetedTextPreservesUnicode()
        print("PASS testTargetedTextPreservesUnicode")
        try suite.testTargetedRepeatedClickPreservesCount()
        print("PASS testTargetedRepeatedClickPreservesCount")
        try suite.testProductionMouseMovementKeepsUserPointerIndependent()
        print("PASS testProductionMouseMovementKeepsUserPointerIndependent")
        try suite.testRecoveryPreservesQuartzUnicodePayload()
        print("PASS testRecoveryPreservesQuartzUnicodePayload")
        print("40 helper regression tests passed")
    }

    private func targetedFixture() -> TargetedInput.Observation {
        .init(
            target: .init(
                pid: 123, bundleId: "dev.fixture", launchedAt: 1234,
                windowId: 42, frame: CGRect(x: 10, y: 20, width: 400, height: 300)),
            frontmostPid: 456, pointer: CGPoint(x: 900, y: 700))
    }

    func testTargetedInputUsesPrivateStateAndBoundWindow() throws {
        let observed = targetedFixture()
        var received: [CGEvent] = []
        let session = try TargetedInput.Session(
            target: observed.target, observation: { observed },
            checkCancellation: {},
            deliver: { event, pid in
                expectEqual(pid, observed.target.pid)
                received.append(event)
            }, pause: { _ in })
        let point = CGPoint(x: 100, y: 120)
        let down = try session.mouse(.leftMouseDown, at: point)
        let up = try session.mouse(.leftMouseUp, at: point)
        expectEqual(CGEventSource(event: down)?.sourceStateID, session.source.sourceStateID)
        expectFalse(session.source.sourceStateID == .combinedSessionState)
        expectFalse(session.source.sourceStateID == .hidSystemState)
        expectEqual(NSEvent(cgEvent: down)?.windowNumber, 42)
        expectEqual(down.location, point)
        expectEqual(EventWindowLocation.get(down), CGPoint(x: 90, y: 100))
        expectEqual(down.getIntegerValueField(.eventTargetUnixProcessID), 123)
        expectEqual(down.getIntegerValueField(.mouseEventWindowUnderMousePointer), 42)
        expectEqual(
            down.getIntegerValueField(.mouseEventWindowUnderMousePointerThatCanHandleThisEvent), 42)
        let key = try session.key(0, down: true, characters: "a")
        expectEqual(CGEventSource(event: key)?.sourceStateID, session.source.sourceStateID)
        expectEqual(NSEvent(cgEvent: key)?.windowNumber, 42)
        let endpoint = CGPoint(x: 150, y: 180)
        let move = try session.mouse(.leftMouseDragged, at: endpoint)
        let result = try session.run([
            .init(event: down, release: up, delay: 0.02),
            .init(event: move, release: nil, delay: 0.02),
        ])
        expectEqual(result.delivered, 2)
        expectEqual(result.released, 1)
        expectEqual(received.map { $0.type }, [.leftMouseDown, .leftMouseDragged, .leftMouseUp])
        expectEqual(received.last?.location, endpoint)
        expectEqual(EventWindowLocation.get(received.last!), CGPoint(x: 140, y: 160))
    }

    func testProductionMouseMovementKeepsUserPointerIndependent() throws {
        try recoveryFixture { request, _ in
            let baseline = targetedFixture()
            var observed = baseline
            var received: [CGEvent] = []
            let session = try TargetedInput.Session(
                target: baseline.target, observation: { observed },
                checkCancellation: {},
                deliver: { event, pid in
                    expectEqual(pid, baseline.target.pid)
                    received.append(event)
                },
                pause: { _ in
                    observed = .init(target: baseline.target, frontmostPid: 789, pointer: .zero)
                }, requireUnchangedDesktop: false)
            let point = CGPoint(x: 100, y: 120)
            let event = try session.mouse(.mouseMoved, at: point)
            try session.perform([.init(event: event, release: nil, delay: 0.02)], request: request)
            expectEqual(received.map { $0.type }, [.mouseMoved])
            expectEqual(received.first?.location, point)
            expectEqual(NSEvent(cgEvent: received.first!)?.windowNumber, 42)
            expectTrue(try InputRecovery.load(request).held.isEmpty)
        }
    }

    func testTargetedRepeatedClickPreservesCount() throws {
        try recoveryFixture { request, _ in
            let baseline = targetedFixture()
            var received: [(CGEventType, Int)] = []
            let session = try TargetedInput.Session(
                target: baseline.target, observation: { baseline },
                checkCancellation: {},
                deliver: { event, _ in
                    received.append((event.type, NSEvent(cgEvent: event)?.clickCount ?? -1))
                }, pause: { _ in }, requireUnchangedDesktop: false)
            var steps: [TargetedInput.Step] = []
            let point = CGPoint(x: 100, y: 120)
            for click in 1...3 {
                let down = try session.mouse(.leftMouseDown, at: point, clickCount: Int64(click))
                let up = try session.mouse(.leftMouseUp, at: point, clickCount: Int64(click))
                steps += [
                    .init(event: down, release: up, delay: 0.02),
                    .init(event: up, release: nil, delay: 0.05),
                ]
            }
            try session.perform(steps, request: request)
            expectEqual(
                received.map { $0.0 },
                [
                    .leftMouseDown, .leftMouseUp, .leftMouseDown,
                    .leftMouseUp, .leftMouseDown, .leftMouseUp,
                ])
            expectEqual(received.map { $0.1 }, [1, 1, 2, 2, 3, 3])
            expectTrue(try InputRecovery.load(request).held.isEmpty)
        }
    }

    func testRecoveryPreservesQuartzUnicodePayload() throws {
        let baseline = targetedFixture()
        let session = try TargetedInput.Session(
            target: baseline.target, observation: { baseline },
            checkCancellation: {}, deliver: { _, _ in }, pause: { _ in })
        for text in ["é", "界", "🙂"] {
            let normal = try session.key(0, down: false, characters: text)
            let held = InputRecovery.Held(
                kind: "key", code: 0, releaseText: text,
                releaseTextIgnoringModifiers: text, releaseFlags: 0)
            let recovered = try InputRecovery.releaseEvent(
                held, target: baseline.target, source: session.source)
            func unicode(_ event: CGEvent) -> [UniChar] {
                var units = [UniChar](repeating: 0, count: 8)
                var length = 0
                event.keyboardGetUnicodeString(
                    maxStringLength: units.count,
                    actualStringLength: &length, unicodeString: &units)
                return Array(units.prefix(length))
            }
            expectEqual(unicode(recovered), unicode(normal))
            expectEqual(unicode(recovered), Array(text.utf16))
            expectEqual(NSEvent(cgEvent: recovered)?.windowNumber, 42)
        }
    }

    func testTargetedTextPreservesUnicode() throws {
        let baseline = targetedFixture()
        let session = try TargetedInput.Session(
            target: baseline.target, observation: { baseline },
            checkCancellation: {}, deliver: { _, _ in }, pause: { _ in })
        let text = "aA! é e\u{301} 😀 中"
        let steps = try session.textSteps(text)
        expectEqual(
            steps.filter { $0.event.type == .keyDown }.compactMap {
                NSEvent(cgEvent: $0.event)?.characters
            }.joined(), text)
        expectTrue(steps.allSatisfy { NSEvent(cgEvent: $0.event)?.windowNumber == 42 })
        let recovered = steps.filter { $0.event.type == .keyDown }.map { step in
            var units = [UniChar](repeating: 0, count: 8)
            var length = 0
            step.event.keyboardGetUnicodeString(
                maxStringLength: units.count,
                actualStringLength: &length, unicodeString: &units)
            return String(utf16CodeUnits: units, count: length)
        }.joined()
        expectEqual(recovered, text)
        expectError(try session.textSteps(String(repeating: "a", count: 501)))
    }

    func testProductionRejectsBeforeDownWithoutRelease() throws {
        try recoveryFixture { request, _ in
            let baseline = targetedFixture()
            var validations = 0
            var received: [CGEventType] = []
            let session = try TargetedInput.Session(
                target: baseline.target, observation: { baseline },
                checkCancellation: {
                    validations += 1
                    if validations == 2 { throw HelperError(code: .yielded, message: "stopped") }
                }, deliver: { event, _ in received.append(event.type) }, pause: { _ in },
                requireUnchangedDesktop: false)
            let point = CGPoint(x: 100, y: 120)
            let down = try session.mouse(.leftMouseDown, at: point)
            let up = try session.mouse(.leftMouseUp, at: point)
            expectError(
                try session.perform([.init(event: down, release: up, delay: 0)], request: request))
            expectTrue(received.isEmpty)
            expectTrue(try InputRecovery.load(request).held.isEmpty)
        }
    }

    func testProductionDragCancellationReleasesWithoutTakeover() throws {
        for replaceTarget in [false, true] {
            try recoveryFixture { request, _ in
                let baseline = targetedFixture()
                var observed = baseline
                var cancelled = false
                var received: [CGEvent] = []
                let session = try TargetedInput.Session(
                    target: baseline.target, observation: { observed },
                    checkCancellation: {
                        if cancelled { throw HelperError(code: .yielded, message: "stopped") }
                    },
                    deliver: { event, pid in
                        expectEqual(pid, baseline.target.pid)
                        received.append(event)
                    },
                    pause: { _ in
                        cancelled = true
                        if replaceTarget {
                            observed = .init(
                                target: .init(
                                    pid: baseline.target.pid,
                                    bundleId: baseline.target.bundleId, launchedAt: 9876,
                                    windowId: baseline.target.windowId, frame: baseline.target.frame
                                ),
                                frontmostPid: 789, pointer: .zero)
                        } else {
                            observed = .init(
                                target: baseline.target, frontmostPid: 789, pointer: .zero)
                        }
                    }, requireUnchangedDesktop: false)
                let point = CGPoint(x: 100, y: 120)
                let down = try session.mouse(.leftMouseDown, at: point)
                let up = try session.mouse(.leftMouseUp, at: point)
                let move = try session.mouse(.leftMouseDragged, at: CGPoint(x: 150, y: 180))
                do {
                    try session.perform(
                        [
                            .init(event: down, release: up, delay: 0.02),
                            .init(event: move, release: nil, delay: 0),
                        ], request: request)
                    fatalError("cancelled input must fail")
                } catch let error as HelperError {
                    expectEqual(error.code, .operationFailed)
                }
                expectEqual(
                    received.map { $0.type },
                    replaceTarget ? [.leftMouseDown] : [.leftMouseDown, .leftMouseUp])
                expectEqual(try InputRecovery.load(request).held.count, replaceTarget ? 1 : 0)
                if !replaceTarget {
                    expectEqual(received.last?.location, point)
                    expectEqual(EventWindowLocation.get(received.last!), CGPoint(x: 90, y: 100))
                }
            }
        }
    }

    func testTargetedScrollPreservesDeltasAndWindow() throws {
        let observed = targetedFixture()
        let session = try TargetedInput.Session(
            target: observed.target, observation: { observed },
            checkCancellation: {}, deliver: { _, _ in }, pause: { _ in })
        let point = CGPoint(x: 100, y: 120)
        for (dx, dy) in [(120.0, -45.0), (-33.0, 140.0)] {
            let reference = try requireValue(
                CGEvent(
                    scrollWheelEvent2Source: session.source,
                    units: .pixel, wheelCount: 2, wheel1: Control.scrollWheelDelta(dy),
                    wheel2: Control.scrollWheelDelta(dx), wheel3: 0))
            let expected = try requireValue(NSEvent(cgEvent: reference))
            let event = try session.scroll(at: point, dx: dx, dy: dy)
            let actual = try requireValue(NSEvent(cgEvent: event))
            expectEqual(event.type, .scrollWheel)
            expectEqual(actual.scrollingDeltaX, expected.scrollingDeltaX)
            expectEqual(actual.scrollingDeltaY, expected.scrollingDeltaY)
            expectEqual(actual.hasPreciseScrollingDeltas, expected.hasPreciseScrollingDeltas)
            expectEqual(actual.windowNumber, 42)
            expectEqual(EventWindowLocation.get(event), CGPoint(x: 90, y: 100))
            expectEqual(event.location, point)
        }
    }

    func testTargetedKeysPreserveModifierTranslation() throws {
        let observed = targetedFixture()
        let session = try TargetedInput.Session(
            target: observed.target, observation: { observed },
            checkCancellation: {}, deliver: { _, _ in }, pause: { _ in })
        for code: CGKeyCode in [0, 18] {
            for flags: CGEventFlags in [.maskShift, [.maskShift, .maskCommand], .maskAlternate] {
                let reference = try requireValue(
                    CGEvent(
                        keyboardEventSource: session.source,
                        virtualKey: code, keyDown: true))
                reference.flags = flags
                let expected = try requireValue(NSEvent(cgEvent: reference))
                for down in [true, false] {
                    let targeted = try session.key(code, down: down, flags: flags)
                    let actual = try requireValue(NSEvent(cgEvent: targeted))
                    expectEqual(actual.characters, expected.characters)
                    expectEqual(
                        actual.charactersIgnoringModifiers, expected.charactersIgnoringModifiers)
                    expectEqual(actual.windowNumber, 42)
                    expectEqual(targeted.flags, flags)
                }
            }
        }
    }

    func testTargetedInputStopsOnFocusAndPointerChangesAndReleases() throws {
        for focusChange in [false, true] {
            let baseline = targetedFixture()
            var observed = baseline
            var received: [CGEventType] = []
            let session = try TargetedInput.Session(
                target: baseline.target, observation: { observed },
                checkCancellation: {}, deliver: { event, _ in received.append(event.type) },
                pause: { _ in
                    observed = .init(
                        target: baseline.target,
                        frontmostPid: focusChange ? baseline.target.pid : baseline.frontmostPid,
                        pointer: focusChange ? baseline.pointer : .zero)
                })
            let down = try session.mouse(.leftMouseDown, at: CGPoint(x: 100, y: 120))
            let up = try session.mouse(.leftMouseUp, at: CGPoint(x: 100, y: 120))
            let move = try session.mouse(.leftMouseDragged, at: CGPoint(x: 150, y: 120))
            expectError(
                try session.run([
                    .init(event: down, release: up, delay: 0.02),
                    .init(event: move, release: nil, delay: 0.02),
                ]))
            expectEqual(received, [.leftMouseDown, .leftMouseUp])
        }
    }

    func testTargetedInputRejectsPIDReuseAndCancellation() throws {
        let baseline = targetedFixture()
        for replaceTarget in [false, true] {
            var observed = baseline
            var cancelled = false
            var received: [CGEventType] = []
            let session = try TargetedInput.Session(
                target: baseline.target, observation: { observed },
                checkCancellation: { if cancelled { throw TestFailure.expectedValue } },
                deliver: { event, _ in received.append(event.type) },
                pause: { _ in
                    cancelled = true
                    if replaceTarget {
                        observed = .init(
                            target: .init(
                                pid: baseline.target.pid,
                                bundleId: baseline.target.bundleId, launchedAt: 9876,
                                windowId: baseline.target.windowId, frame: baseline.target.frame),
                            frontmostPid: baseline.frontmostPid, pointer: baseline.pointer)
                    }
                })
            let down = try session.key(0, down: true, characters: "a")
            let up = try session.key(0, down: false, characters: "a")
            expectError(
                try session.run([
                    .init(event: down, release: up, delay: 0.02),
                    .init(event: down, release: nil, delay: 0.02),
                ]))
            expectEqual(received, replaceTarget ? [.keyDown] : [.keyDown, .keyUp])
        }
    }

    func testTargetedJournalRetainsDestinationAndReleasePayload() throws {
        try recoveryFixture { request, _ in
            let observed = targetedFixture()
            let session = try TargetedInput.Session(
                target: observed.target, observation: { observed },
                checkCancellation: {}, deliver: { _, _ in }, pause: { _ in })
            let down = try session.key(0, down: true, flags: .maskShift)
            var received: [(CGEventType, pid_t)] = []
            try InputRecovery.post(
                down, request: request, target: observed.target,
                deliver: { event, pid in received.append((event.type, pid)) })
            let journal = try InputRecovery.load(request)
            expectEqual(journal.target, observed.target)
            let held = try requireValue(journal.held.first)
            expectEqual(held.releaseText, NSEvent(cgEvent: down)?.characters)
            expectEqual(
                held.releaseTextIgnoringModifiers,
                NSEvent(cgEvent: down)?.charactersIgnoringModifiers)
            expectEqual(held.releaseFlags, CGEventFlags.maskShift.rawValue)
            let release = try InputRecovery.releaseEvent(
                held, target: observed.target,
                source: session.source)
            expectEqual(release.type, .keyUp)
            expectEqual(release.getIntegerValueField(.eventTargetUnixProcessID), 123)
            expectEqual(NSEvent(cgEvent: release)?.windowNumber, 42)
            expectEqual(release.flags, .maskShift)
            expectEqual(NSEvent(cgEvent: release)?.characters, NSEvent(cgEvent: down)?.characters)
            expectEqual(
                NSEvent(cgEvent: release)?.charactersIgnoringModifiers,
                NSEvent(cgEvent: down)?.charactersIgnoringModifiers)
            let recovered = try InputRecovery.recover(
                journal: journal, request: request,
                physicallyHeld: { _ in false },
                release: { control in
                    let event = try InputRecovery.releaseEvent(
                        control, target: observed.target,
                        source: session.source)
                    received.append((event.type, observed.target.pid))
                })
            expectEqual(recovered.released, 1)
            expectEqual(received.map { $0.0 }, [.keyDown, .keyUp])
            expectTrue(received.allSatisfy { $0.1 == 123 })
            expectTrue(try InputRecovery.load(request).held.isEmpty)
            let replaced = TargetedInput.Target(
                pid: 123, bundleId: "dev.fixture", launchedAt: 9999,
                windowId: 42, frame: observed.target.frame)
            expectFalse(InputRecovery.sameDestination(replaced, observed.target))
        }
    }

    func testTargetedMouseRecoveryUsesLastAgentPosition() throws {
        try recoveryFixture { request, _ in
            let observed = targetedFixture()
            let session = try TargetedInput.Session(
                target: observed.target, observation: { observed },
                checkCancellation: {}, deliver: { _, _ in }, pause: { _ in })
            let down = try session.mouse(.leftMouseDown, at: CGPoint(x: 100, y: 120))
            let endpoint = CGPoint(x: 150, y: 180)
            let move = try session.mouse(.leftMouseDragged, at: endpoint)
            var received: [CGEventType] = []
            let deliver: (CGEvent, pid_t) -> Void = { event, pid in
                expectEqual(pid, observed.target.pid)
                received.append(event.type)
            }
            expectError(
                try InputRecovery.post(
                    move, request: request, target: observed.target,
                    deliver: deliver))
            expectTrue(received.isEmpty)
            try InputRecovery.post(
                down, request: request, target: observed.target, deliver: deliver)
            try InputRecovery.post(
                move, request: request, target: observed.target, deliver: deliver)
            let journal = try InputRecovery.load(request)
            let held = try requireValue(journal.held.first)
            let release = try InputRecovery.releaseEvent(
                held, target: observed.target, source: session.source)
            expectEqual(received, [.leftMouseDown, .leftMouseDragged])
            expectEqual(release.location, endpoint)
            expectEqual(EventWindowLocation.get(release), CGPoint(x: 140, y: 160))
            expectEqual(NSEvent(cgEvent: release)?.windowNumber, 42)
            expectEqual(CGEventSource(event: release)?.sourceStateID, session.source.sourceStateID)
            expectEqual(release.getIntegerValueField(.eventTargetUnixProcessID), 123)
        }
    }

    func testProductionTargetAllowsUnrelatedPointerAndAppChanges() throws {
        let baseline = targetedFixture()
        var observed = baseline
        let session = try TargetedInput.Session(
            target: baseline.target, observation: { observed },
            checkCancellation: {}, deliver: { _, _ in }, pause: { _ in },
            requireUnchangedDesktop: false)
        observed = .init(target: baseline.target, frontmostPid: 789, pointer: .zero)
        expectSuccess(try session.validate())
        observed = .init(target: baseline.target, frontmostPid: baseline.target.pid, pointer: .zero)
        expectError(try session.validate())
        expectSuccess(try session.validate(releasing: true))
    }

    private func recoveryFixture(_ work: (HelperRequest, URL) throws -> Void) throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("tidebreak-recovery-test-" + UUID().uuidString)
        try FileManager.default.createDirectory(
            at: directory, withIntermediateDirectories: false,
            attributes: [.posixPermissions: 0o700])
        defer { try? FileManager.default.removeItem(at: directory) }
        let invocation = UUID().uuidString
        let journal = directory.appendingPathComponent("input.json")
        let cancel = directory.appendingPathComponent("cancel")
        let data = try JSONSerialization.data(withJSONObject: [
            "invocation_id": invocation, "held": [],
        ])
        try data.write(to: journal)
        try Data(invocation.utf8).write(to: cancel)
        try FileManager.default.setAttributes(
            [.posixPermissions: 0o600], ofItemAtPath: journal.path)
        let requestData = try JSONSerialization.data(withJSONObject: [
            "op": "drag",
            "input_invocation_id": invocation, "input_journal_path": journal.path,
            "input_cancel_path": cancel.path,
        ])
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        try work(decoder.decode(HelperRequest.self, from: requestData), journal)
    }

    func testModifierAndMouseEventsHaveTrackedReleases() throws {
        let source = try requireValue(CGEventSource(stateID: .combinedSessionState))
        for key: CGKeyCode in [0, 54, 55, 56, 60, 58, 61, 59, 62, 63] {
            for down in [true, false] {
                let event = try requireValue(
                    CGEvent(keyboardEventSource: source, virtualKey: key, keyDown: down))
                let tracked = try requireValue(InputRecovery.heldControl(event))
                expectEqual(tracked.0, InputRecovery.Held(kind: "key", code: key))
                expectEqual(tracked.1, down)
            }
        }
        for (eventType, button, down) in [
            (CGEventType.leftMouseDown, CGMouseButton.left, true),
            (.leftMouseUp, .left, false), (.rightMouseDown, .right, true),
            (.rightMouseUp, .right, false),
        ] {
            let event = try requireValue(
                CGEvent(
                    mouseEventSource: source, mouseType: eventType,
                    mouseCursorPosition: .zero, mouseButton: button))
            let tracked = try requireValue(InputRecovery.heldControl(event))
            expectEqual(tracked.0, InputRecovery.Held(kind: "mouse", code: UInt16(button.rawValue)))
            expectEqual(tracked.1, down)
        }
    }

    func testInputJournalSurvivesUntilRelease() throws {
        try recoveryFixture { request, _ in
            let mouse = InputRecovery.Held(kind: "mouse", code: 0)
            try InputRecovery.track(
                mouse, down: true, request: request,
                deliver: {
                    expectEqual(try! InputRecovery.load(request).held, [mouse])
                })
            try InputRecovery.track(
                mouse, down: false, request: request,
                deliver: {
                    expectEqual(try! InputRecovery.load(request).held, [mouse])
                })
            expectTrue(try InputRecovery.load(request).held.isEmpty)
        }
    }

    func testRecoveryPreservesPhysicalHoldsAndReleasesInReverseOrder() throws {
        try recoveryFixture { request, _ in
            let command = InputRecovery.Held(kind: "key", code: 55)
            let key = InputRecovery.Held(kind: "key", code: 0)
            let mouse = InputRecovery.Held(kind: "mouse", code: 0)
            for control in [command, key, mouse] {
                try InputRecovery.track(control, down: true, request: request, deliver: {})
            }
            var released: [InputRecovery.Held] = []
            let result = try InputRecovery.recover(
                journal: InputRecovery.load(request), request: request,
                physicallyHeld: { $0 == mouse }, release: { released.append($0) })
            expectEqual(released, [key, command])
            expectEqual(result.released, 2)
            expectEqual(result.preservedPhysicalHolds, 1)
            expectTrue(try InputRecovery.load(request).held.isEmpty)
        }
    }

    func testRecoveryRejectsWrongIdentityOversizedAndPublicJournals() throws {
        try recoveryFixture { request, path in
            let initial = try Data(contentsOf: path)
            let wrong = InputRecovery.Journal(invocationId: UUID().uuidString, held: [])
            try InputRecovery.save(wrong, request: request)
            expectError(try InputRecovery.load(request))
            try Data(repeating: 65, count: 16 * 1024 + 1).write(to: path)
            expectError(try InputRecovery.load(request))
            try initial.write(to: path)
            try FileManager.default.setAttributes(
                [.posixPermissions: 0o644], ofItemAtPath: path.path)
            expectError(try InputRecovery.load(request))
        }
    }

    func testUpStillReleasesAfterJournalDamage() throws {
        try recoveryFixture { request, path in
            let mouse = InputRecovery.Held(kind: "mouse", code: 0)
            try InputRecovery.track(mouse, down: true, request: request, deliver: {})
            try Data("broken".utf8).write(to: path)
            var released = false
            expectError(
                try InputRecovery.track(
                    mouse, down: false, request: request, deliver: { released = true }))
            expectTrue(released)
        }
    }

    func testInvocationCancellationRefusesFurtherInput() throws {
        try recoveryFixture { request, _ in
            try InputRecovery.checkCancellation(request)
            try Data("stopped".utf8).write(to: URL(fileURLWithPath: request.inputCancelPath!))
            expectError(try InputRecovery.checkCancellation(request)) { error in
                expectEqual((error as? HelperError)?.code, .yielded)
            }
        }
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

    func testBackgroundIsDefaultAndForegroundOnlyInputRefusesEarly() throws {
        let command = try request(#"{"op":"hover"}"#)
        expectNil(command.executionMode)
        let calls: [(HelperRequest) throws -> Control.Result] = [
            Control.focusWindow
        ]
        for call in calls {
            expectError(try call(command)) { error in
                expectEqual((error as? HelperError)?.code, .independentInputUnavailable)
            }
        }
        let foreground = try request(#"{"op":"hover","execution_mode":"foreground"}"#)
        expectError(try Control.requireForeground(foreground, operation: "hover"))
        for operation in [
            "click", "type_text", "key_press", "scroll", "focus_window", "launch_app", "hover",
            "drag",
            "resize_window",
        ] {
            let legacy = try request("{\"op\":\"\(operation)\",\"execution_mode\":\"foreground\"}")
            expectError(try legacy.requireIndependentInput()) { error in
                expectEqual((error as? HelperError)?.code, .independentInputUnavailable)
            }
        }
        let recovery = try request(
            #"{"op":"release_recorded_input","execution_mode":"foreground"}"#)
        expectSuccess(try recovery.requireIndependentInput())
    }

    func testExecutionModeIsExplicitInControlResults() throws {
        let result = Control.Result(
            executionMode: .background, success: true, usedFallback: false, detail: "AXPress")
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        let value = try requireValue(
            JSONSerialization.jsonObject(with: encoder.encode(result)) as? [String: Any])
        expectEqual(value["execution_mode"] as? String, "background")
    }

    func testBackgroundScrollClampsAtEachEnd() {
        expectEqual(Control.backgroundScrollPosition(current: 0, delta: -180), 0)
        expectEqual(Control.backgroundScrollPosition(current: 1, delta: 180), 1)
        expectEqual(Control.backgroundScrollPosition(current: 0, delta: 180), 0.1)
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

    func testPartialDragReportsUncertainOutcome() {
        for code in [HelperErrorCode.targetOutsideApp, .yielded, .invalidRequest] {
            var events: [String] = []
            expectError(
                try Control.deliverDrag(
                    count: 3,
                    press: { events.append("press") },
                    move: { index in
                        if index == 1 {
                            throw HelperError(code: code, message: "fixture guard refused")
                        }
                        events.append("move\(index)")
                    },
                    release: { events.append("release") }, pause: {})
            ) { error in
                expectEqual((error as? HelperError)?.code, .operationFailed)
            }
            expectEqual(events, ["press", "move0", "release"])
        }
    }

    func testActivationWaitsForTheFrontmostApp() throws {
        var polls = 0
        try Control.waitForActivation(
            timeout: 0.6, isFrontmost: { polls >= 3 }, check: {},
            now: { Double(polls) * 0.01 }, pause: { polls += 1 })
        expectEqual(polls, 3)
    }

    func testActivationWaitIsBoundedAndCancellable() {
        var time: TimeInterval = 0
        expectError(
            try Control.waitForActivation(
                timeout: 0.6, isFrontmost: { false }, check: {},
                now: { time }, pause: { time += 0.1 })
        ) { error in
            expectEqual((error as? HelperError)?.code, .yielded)
        }
        expectAtLeast(time, 0.6)
        expectAtMost(time, 0.7)

        var polls = 0
        expectError(
            try Control.waitForActivation(
                timeout: 0.6, isFrontmost: { false },
                check: {
                    if polls == 1 {
                        throw HelperError(code: .yielded, message: "fixture stopped")
                    }
                },
                now: { Double(polls) * 0.01 }, pause: { polls += 1 })
        ) { error in
            expectEqual((error as? HelperError)?.code, .yielded)
        }
        expectEqual(polls, 1)
    }

    func testSuccessfulDragReleasesExactlyOnce() throws {
        var events: [String] = []
        try Control.deliverDrag(
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
            expectEqual((error as? HelperError)?.code, .operationFailed)
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
