import Foundation

enum FixtureError: Error, CustomStringConvertible {
    case unsupportedMacOS
    case missingFixtureDirectory
    case unresolvableRunID
    case runIDAlreadyUsed

    var description: String {
        switch self {
        case .unsupportedMacOS:
            return "Computer Use Fixture requires macOS 13 or newer"
        case .missingFixtureDirectory:
            return "Pass --fixture-dir <directory> or TIDEBREAK_CU_FIXTURE_DIR"
        case .unresolvableRunID:
            return "Could not build a unique run id"
        case .runIDAlreadyUsed:
            return "The requested run id already has fixture events; choose a fresh run id"
        }
    }
}

// JSON events are written one object per file so a crash never leaves a
// half-written record and a replay cannot silently reuse a sequence number.
final class EventStore {
    let eventsDirectory: URL
    private let runID: String
    private(set) var sequence = 0
    private let serialQueue = DispatchQueue(label: "dev.tidebreak.ComputerUseFixture.events")
    private let dateFormatter = ISO8601DateFormatter()

    init(fixtureDirectory: URL, runID: String) throws {
        let eventsDir = fixtureDirectory
            .appendingPathComponent("events", isDirectory: true)
            .appendingPathComponent(runID, isDirectory: true)
        let manager = FileManager.default
        if manager.fileExists(atPath: eventsDir.path),
            let existing = try? manager.contentsOfDirectory(at: eventsDir, includingPropertiesForKeys: nil),
            !existing.isEmpty {
            throw FixtureError.runIDAlreadyUsed
        }
        try manager.createDirectory(at: eventsDir, withIntermediateDirectories: true)
        self.eventsDirectory = eventsDir
        self.runID = runID
        dateFormatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    }

    func write(_ event: String, payload: [String: Any]) {
        serialQueue.sync {
            self.sequence += 1
            let fileName = String(format: "%06d.json", self.sequence)
            let url = eventsDirectory.appendingPathComponent(fileName)
            let object: [String: Any] = [
                "event": event,
                "run_id": self.runID,
                "timestamp": dateFormatter.string(from: Date()),
                "sequence": self.sequence,
                "payload": payload,
            ]
            do {
                let data = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
                try data.write(to: url, options: [.atomic])
            } catch {
                NSLog("ComputerUseFixture: could not write %@: %@", fileName, String(describing: error))
            }
        }
    }
}

