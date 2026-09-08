// Computer Use Fixture — bounded AppKit acceptance surface for Tidebreak's
// native computer-use integration.
//
// This application exists to prove that real macOS accessibility events
// happened. It has no network access beyond the OS's own loopback interfaces,
// no shell execution, no secrets, no access to arbitrary host files, and no
// approval surface. Every confirmed event is written as one bounded JSON
// record to a caller-selected directory.

import AppKit
import Foundation

let fixtureBundleID = "dev.tidebreak.ComputerUseFixture"
let fixtureWindowTitle = "Computer Use Fixture"
let requiredMacOSVersion = OperatingSystemVersion(majorVersion: 13, minorVersion: 0, patchVersion: 0)

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

struct AppArguments {
    var fixtureDirectory: URL
    var runID: String
}

// JSON events are written one object per file so a crash never leaves a
// half-written record and a replay cannot silently reuse a sequence number.
final class EventStore {
    let fixtureDirectory: URL
    let eventsDirectory: URL
    private let runID: String
    private(set) var sequence = 0
    private let serialQueue = DispatchQueue(label: "dev.tidebreak.ComputerUseFixture.events")
    private let dateFormatter = ISO8601DateFormatter()

    init(fixtureDirectory: URL, runID: String) throws {
        self.fixtureDirectory = fixtureDirectory
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
            var object: [String: Any] = [
                "event": event,
                "run_id": self.runID,
                "timestamp": dateFormatter.string(from: Date()),
                "sequence": self.sequence,
            ]
            for (key, value) in payload {
                object[key] = value
            }
            do {
                let data = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
                try data.write(to: url, options: [.atomic])
            } catch {
                NSLog("ComputerUseFixture: could not write %@: %@", fileName, String(describing: error))
            }
        }
    }
}

// Bounded, inspectable fixture state. All state is intentionally observable
// through the accessibility tree, so a native helper can read it back.
final class FixtureState {
    let runID: String
    private let store: EventStore
    private var submissionCounter = 0
    private var dropdownSelection = "First"
    private var checkboxChecked = false
    private var hovered = false
    private var dragDropped = false
    private var delayedStatus = "idle"
    private var secondWindowOpen = false
    private var lastDragTarget = "none"
    private var scrollOffset = 0.0
    private var windowWidth = 920.0
    private var windowHeight = 760.0

    init(store: EventStore, runID: String) {
        self.store = store
        self.runID = runID
        store.write("launch_ready", payload: ["app_id": fixtureBundleID, "title": fixtureWindowTitle])
    }

    func submissionCount() -> Int { submissionCounter }
    func selectedDropdown() -> String { dropdownSelection }
    func isCheckboxChecked() -> Bool { checkboxChecked }
    func isHovered() -> Bool { hovered }
    func wasDragDropped() -> Bool { dragDropped }
    func dragTarget() -> String { lastDragTarget }
    func currentDelayedStatus() -> String { delayedStatus }
    func isSecondWindowOpen() -> Bool { secondWindowOpen }

    func snapshot() {
        snapshot(runID: runID)
    }

    private func snapshot(runID overriddenRunID: String) {
        store.write("state_snapshot", payload: [
            "app_id": fixtureBundleID,
            "title": fixtureWindowTitle,
            "run_id": overriddenRunID,
            "submission_count": submissionCounter,
            "dropdown": dropdownSelection,
            "checkbox": checkboxChecked,
            "hovered": hovered,
            "drag_dropped": dragDropped,
            "drag_target": lastDragTarget,
            "delayed_status": delayedStatus,
            "second_window_open": secondWindowOpen,
            "scroll_offset": Int(scrollOffset),
            "window_size": ["width": Int(windowWidth), "height": Int(windowHeight)],
        ])
    }

    func incrementSubmissions() {
        submissionCounter += 1
        store.write("submission", payload: ["count": submissionCounter])
        snapshot()
    }

    func selectDropdown(_ value: String) {
        dropdownSelection = value
        store.write("dropdown_selection", payload: ["value": value])
        snapshot()
    }

    func setCheckbox(_ checked: Bool) {
        checkboxChecked = checked
        store.write("checkbox_toggle", payload: ["checked": checked])
        snapshot()
    }

    func setHovered(_ value: Bool) {
        hovered = value
        store.write("hover_status", payload: ["hovered": value])
        snapshot()
    }

    func markDragStarted(_ target: String) {
        store.write("drag_started", payload: ["target": target])
        snapshot()
    }

    func markDragDropped(on target: String) {
        dragDropped = true
        lastDragTarget = target
        store.write("drag_dropped", payload: ["target": target])
        snapshot()
    }

    func setDelayedStatus(_ value: String) {
        delayedStatus = value
        store.write("delayed_status", payload: ["status": value])
        snapshot()
    }

    func setSecondWindowOpen(_ open: Bool) {
        secondWindowOpen = open
        store.write(open ? "second_window_opened" : "second_window_closed", payload: ["open": open])
        snapshot()
    }

    func noteWindowResized(width: Double, height: Double) {
        windowWidth = width
        windowHeight = height
        store.write("window_resized", payload: ["width": Int(width), "height": Int(height)])
        snapshot()
    }

    func noteScroll(contentY: Double) {
        scrollOffset = max(0, contentY)
        store.write("scroll", payload: ["content_y": Int(scrollOffset)])
        snapshot()
    }

    func noteTextEntry(_ value: String) {
        store.write("text_entry", payload: ["value": value])
        snapshot()
    }

    func markResetRequested(newRunID: String) {
        store.write("reset_requested", payload: ["new_run_id": newRunID])
    }

    /// The old run's final snapshot describes the freshly reset run so the
    /// accepting runner can observe the transition from the directory it
    /// already owns. Events after this point belong to the new run.
    func markResetCompleted(newRunID: String) {
        store.write("reset_completed", payload: ["new_run_id": newRunID])
        snapshot(runID: newRunID)
    }
}

class FlippedView: NSView {
    override var isFlipped: Bool { true }
}

final class HoverStatusView: NSView {
    var onHoverChanged: ((Bool) -> Void)?
    private var trackingArea: NSTrackingArea?

    override func updateTrackingAreas() {
        if let trackingArea { removeTrackingArea(trackingArea) }
        let options: NSTrackingArea.Options = [.activeInKeyWindow, .inVisibleRect, .mouseEnteredAndExited]
        let area = NSTrackingArea(rect: .zero, options: options, owner: self, userInfo: nil)
        addTrackingArea(area)
        trackingArea = area
        super.updateTrackingAreas()
    }

    override func mouseEntered(with event: NSEvent) {
        onHoverChanged?(true)
    }

    override func mouseExited(with event: NSEvent) {
        onHoverChanged?(false)
    }
}

final class DragItemView: NSView {
    var onDragStarted: ((String) -> Void)?
    var onDrop: ((String) -> Void)?
    private var dragOffset: NSPoint?

    override func mouseDown(with event: NSEvent) {
        dragOffset = convert(event.locationInWindow, from: nil)
        onDragStarted?(accessibilityIdentifier() ?? "fixture-drag-item")
    }

    override func mouseDragged(with event: NSEvent) {
        guard let dragOffset, let root = superview else { return }
        let point = root.convert(event.locationInWindow, from: nil)
        var next = frame
        next.origin.x = point.x - dragOffset.x
        next.origin.y = point.y - dragOffset.y
        next.origin.x = min(max(next.origin.x, 0), max(0, root.bounds.width - next.width))
        next.origin.y = min(max(next.origin.y, 0), max(0, root.bounds.height - next.height))
        frame = next
        needsDisplay = true
    }

    override func mouseUp(with event: NSEvent) {
        defer { dragOffset = nil }
        guard let root = superview else {
            onDrop?("none")
            return
        }
        let point = root.convert(event.locationInWindow, from: nil)
        guard let hit = root.hitTest(point), hit !== self else {
            onDrop?("none")
            return
        }
        var resolved: NSView? = hit
        while let candidate = resolved {
            if candidate === root || candidate === self {
                resolved = nil
                break
            }
            if candidate.accessibilityIdentifier() == "fixture-drop-target" {
                break
            }
            resolved = candidate.superview
        }
        onDrop?(resolved?.accessibilityIdentifier() ?? "dropped")
    }
}

private func formLabel(_ text: String, width: CGFloat) -> NSTextField {
    let label = NSTextField(labelWithString: text)
    label.font = .systemFont(ofSize: NSFont.systemFontSize)
    label.textColor = .secondaryLabelColor
    let slug = text.lowercased().replacingOccurrences(of: " ", with: "-")
    label.setAccessibilityIdentifier("fixture-form-label-\(slug)")
    label.translatesAutoresizingMaskIntoConstraints = false
    label.widthAnchor.constraint(equalToConstant: width).isActive = true
    return label
}

final class FixtureWindowController: NSWindowController, NSWindowDelegate, NSTextFieldDelegate {
    private let fixtureDirectory: URL
    private var state: FixtureState
    private(set) var currentRunID: String

    private let inputField = NSTextField()
    private let addButton = NSButton(title: "Add", target: nil, action: nil)
    private let submissionCountLabel = NSTextField(labelWithString: "Submissions: 0")
    private let dropdown = NSPopUpButton()
    private let checkbox = NSButton(checkboxWithTitle: "Enable feature", target: nil, action: nil)
    private let hoverView = HoverStatusView()
    private let hoverStatusLabel = NSTextField(labelWithString: "Status: not hovered")
    private let scrollView = NSScrollView()
    private let dragItem = DragItemView()
    private let dropTarget = NSView()
    private let dropTargetLabel = NSTextField(labelWithString: "Drop target: waiting")
    private let delayedStatusButton = NSButton(title: "Start delayed transition", target: nil, action: nil)
    private let delayedStatusLabel = NSTextField(labelWithString: "Delayed status: idle")
    private let secondWindowButton = NSButton(title: "Open second window", target: nil, action: nil)
    private let runIDLabel = NSTextField(labelWithString: "")
    private let resetButton = NSButton(title: "Reset", target: nil, action: nil)
    private var secondWindowController: NSWindowController?
    private var positionedDragItem = false
    private var lastScrollY: CGFloat = 0
    private var delayedWorkItem: DispatchWorkItem?

    init(fixtureDirectory: URL, state: FixtureState) {
        self.fixtureDirectory = fixtureDirectory
        self.state = state
        self.currentRunID = state.runID
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 920, height: 760),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        super.init(window: window)
        window.title = fixtureWindowTitle
        window.setAccessibilityIdentifier("fixture-main-window")
        window.setAccessibilityTitle(fixtureWindowTitle)
        window.minSize = NSSize(width: 760, height: 640)
        window.delegate = self
        buildContent()
        window.center()
        window.makeKeyAndOrderFront(nil)
        window.contentView?.layoutSubtreeIfNeeded()
        positionDragItem()
        state.snapshot()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("unavailable") }

    private func buildContent() {
        guard let window else { return }
        let root = FlippedView()
        root.setAccessibilityIdentifier("fixture-root")
        root.translatesAutoresizingMaskIntoConstraints = false
        window.contentView = root

        let title = NSTextField(labelWithString: fixtureWindowTitle)
        title.font = .boldSystemFont(ofSize: 18)
        title.setAccessibilityIdentifier("fixture-title")
        title.translatesAutoresizingMaskIntoConstraints = false

        let addLabel = formLabel("Add entry", width: 120)
        inputField.placeholderString = "Unique acceptance text"
        inputField.setAccessibilityIdentifier("fixture-text-input")
        inputField.setAccessibilityLabel("Text input")
        inputField.translatesAutoresizingMaskIntoConstraints = false

        addButton.target = self
        addButton.action = #selector(addPressed)
        addButton.setAccessibilityIdentifier("fixture-add-button")
        addButton.setAccessibilityLabel("Add button")
        addButton.keyEquivalent = "\r"
        inputField.delegate = self
        addButton.translatesAutoresizingMaskIntoConstraints = false

        submissionCountLabel.setAccessibilityIdentifier("fixture-submission-count")
        submissionCountLabel.translatesAutoresizingMaskIntoConstraints = false
        updateSubmissionLabel()

        let dropdownLabel = formLabel("Dropdown", width: 120)
        dropdown.addItems(withTitles: ["First", "Second", "Third"])
        dropdown.target = self
        dropdown.action = #selector(dropdownChanged)
        dropdown.setAccessibilityIdentifier("fixture-dropdown")
        dropdown.setAccessibilityLabel("Dropdown selector")
        dropdown.translatesAutoresizingMaskIntoConstraints = false

        let checkboxLabel = formLabel("Checkbox", width: 120)
        checkbox.target = self
        checkbox.action = #selector(checkboxChanged)
        checkbox.setAccessibilityIdentifier("fixture-checkbox")
        checkbox.setAccessibilityLabel("Feature checkbox")
        checkbox.translatesAutoresizingMaskIntoConstraints = false

        let hoverLabel = formLabel("Hover area", width: 120)
        hoverView.onHoverChanged = { [weak self] hovered in
            guard let self else { return }
            self.state.setHovered(hovered)
            self.hoverStatusLabel.stringValue = hovered ? "Status: hovered" : "Status: not hovered"
        }
        hoverView.setAccessibilityIdentifier("fixture-hover-area")
        hoverView.setAccessibilityLabel("Hover status area")
        hoverView.setAccessibilityDescription("Hover enters a mouse tracking area and switches the status")
        hoverView.wantsLayer = true
        hoverView.layer?.backgroundColor = NSColor.controlBackgroundColor.cgColor
        hoverView.layer?.borderWidth = 1
        hoverView.layer?.borderColor = NSColor.separatorColor.cgColor
        hoverView.translatesAutoresizingMaskIntoConstraints = false

        hoverStatusLabel.setAccessibilityIdentifier("fixture-hover-status")
        hoverStatusLabel.translatesAutoresizingMaskIntoConstraints = false

        let scrollLabel = formLabel("Scroll area", width: 120)
        let scrollContent = FlippedView(frame: NSRect(x: 0, y: 0, width: 340, height: 500))
        scrollContent.setAccessibilityIdentifier("fixture-scroll-content")
        for index in 1...20 {
            let row = NSTextField(labelWithString: "Row \(index)")
            row.translatesAutoresizingMaskIntoConstraints = false
            row.setAccessibilityIdentifier("fixture-scroll-row-\(index)")
            row.frame = NSRect(x: 10, y: 8 + CGFloat(index - 1) * 24, width: 300, height: 20)
            scrollContent.addSubview(row)
        }
        scrollView.documentView = scrollContent
        scrollView.hasVerticalScroller = true
        scrollView.drawsBackground = true
        scrollView.setAccessibilityIdentifier("fixture-scroll-area")
        scrollView.setAccessibilityLabel("Scroll area")
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        NotificationCenter.default.addObserver(
            forName: NSView.boundsDidChangeNotification,
            object: scrollView.contentView,
            queue: .main
        ) { [weak self] _ in
            guard let self else { return }
            let y = self.scrollView.contentView.bounds.origin.y
            guard abs(y - self.lastScrollY) > 0.5 else { return }
            self.lastScrollY = y
            self.state.noteScroll(contentY: y)
        }

        let dragLabel = formLabel("Drag item", width: 120)
        dragItem.setAccessibilityIdentifier("fixture-drag-item")
        dragItem.setAccessibilityLabel("Draggable item")
        dragItem.setAccessibilityDescription("Mouse-drag this item onto the drop target")
        dragItem.wantsLayer = true
        dragItem.layer?.backgroundColor = NSColor.systemBlue.withAlphaComponent(0.18).cgColor
        dragItem.layer?.borderWidth = 1
        dragItem.layer?.borderColor = NSColor.systemBlue.cgColor
        let dragItemText = NSTextField(labelWithString: "Drag me")
        dragItemText.translatesAutoresizingMaskIntoConstraints = false
        dragItemText.setAccessibilityIdentifier("fixture-drag-item-text")
        dragItem.addSubview(dragItemText)
        dragItemText.centerXAnchor.constraint(equalTo: dragItem.centerXAnchor).isActive = true
        dragItemText.centerYAnchor.constraint(equalTo: dragItem.centerYAnchor).isActive = true
        dragItem.onDragStarted = { [weak self] identifier in
            self?.state.markDragStarted(identifier)
        }
        dragItem.onDrop = { [weak self] target in
            guard let self else { return }
            self.state.markDragDropped(on: target)
            self.dropTargetLabel.stringValue = target == "fixture-drop-target" ? "Drop target: received" : "Drop target: missed"
        }

        dropTarget.setAccessibilityIdentifier("fixture-drop-target")
        dropTarget.setAccessibilityLabel("Drop target")
        dropTarget.setAccessibilityDescription("Drop the draggable item here")
        dropTarget.wantsLayer = true
        dropTarget.layer?.backgroundColor = NSColor.systemGreen.withAlphaComponent(0.12).cgColor
        dropTarget.layer?.borderWidth = 1
        dropTarget.layer?.borderColor = NSColor.systemGreen.cgColor
        dropTarget.translatesAutoresizingMaskIntoConstraints = false

        dropTargetLabel.setAccessibilityIdentifier("fixture-drop-status")
        dropTargetLabel.translatesAutoresizingMaskIntoConstraints = false

        let delayedLabel = formLabel("Delayed transition", width: 120)
        delayedStatusButton.target = self
        delayedStatusButton.action = #selector(delayedStatusPressed)
        delayedStatusButton.setAccessibilityIdentifier("fixture-delayed-button")
        delayedStatusButton.setAccessibilityLabel("Start delayed transition")
        delayedStatusButton.translatesAutoresizingMaskIntoConstraints = false

        delayedStatusLabel.setAccessibilityIdentifier("fixture-delayed-status")
        delayedStatusLabel.translatesAutoresizingMaskIntoConstraints = false

        let secondWindowLabel = formLabel("Second window", width: 120)
        secondWindowButton.target = self
        secondWindowButton.action = #selector(secondWindowPressed)
        secondWindowButton.setAccessibilityIdentifier("fixture-second-window-button")
        secondWindowButton.setAccessibilityLabel("Open second window")
        secondWindowButton.translatesAutoresizingMaskIntoConstraints = false

        runIDLabel.stringValue = "Run: \(currentRunID)"
        runIDLabel.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
        runIDLabel.textColor = .secondaryLabelColor
        runIDLabel.setAccessibilityIdentifier("fixture-run-id")
        runIDLabel.translatesAutoresizingMaskIntoConstraints = false

        resetButton.target = self
        resetButton.action = #selector(resetPressed)
        resetButton.bezelStyle = .rounded
        resetButton.setAccessibilityIdentifier("fixture-reset-button")
        resetButton.setAccessibilityLabel("Reset fixture")
        resetButton.translatesAutoresizingMaskIntoConstraints = false

        let controls: [NSView] = [
            title, addLabel, inputField, addButton, submissionCountLabel, dropdownLabel, dropdown,
            checkboxLabel, checkbox, hoverLabel, hoverView, hoverStatusLabel, scrollLabel, scrollView,
            dragLabel, dragItem, dropTarget, dropTargetLabel, delayedLabel, delayedStatusButton,
            delayedStatusLabel, secondWindowLabel, secondWindowButton, runIDLabel, resetButton,
        ]
        for control in controls {
            root.addSubview(control)
        }

        NSLayoutConstraint.activate([
            title.topAnchor.constraint(equalTo: root.topAnchor, constant: 16),
            title.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 20),

            addLabel.topAnchor.constraint(equalTo: title.bottomAnchor, constant: 24),
            addLabel.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 20),
            inputField.centerYAnchor.constraint(equalTo: addLabel.centerYAnchor),
            inputField.leadingAnchor.constraint(equalTo: addLabel.trailingAnchor, constant: 10),
            inputField.widthAnchor.constraint(equalToConstant: 360),
            addButton.centerYAnchor.constraint(equalTo: addLabel.centerYAnchor),
            addButton.leadingAnchor.constraint(equalTo: inputField.trailingAnchor, constant: 10),
            submissionCountLabel.centerYAnchor.constraint(equalTo: addLabel.centerYAnchor),
            submissionCountLabel.leadingAnchor.constraint(equalTo: addButton.trailingAnchor, constant: 16),

            dropdownLabel.topAnchor.constraint(equalTo: addLabel.bottomAnchor, constant: 20),
            dropdownLabel.leadingAnchor.constraint(equalTo: addLabel.leadingAnchor),
            dropdown.centerYAnchor.constraint(equalTo: dropdownLabel.centerYAnchor),
            dropdown.leadingAnchor.constraint(equalTo: inputField.leadingAnchor),
            dropdown.widthAnchor.constraint(equalToConstant: 180),

            checkboxLabel.topAnchor.constraint(equalTo: dropdownLabel.bottomAnchor, constant: 20),
            checkboxLabel.leadingAnchor.constraint(equalTo: addLabel.leadingAnchor),
            checkbox.centerYAnchor.constraint(equalTo: checkboxLabel.centerYAnchor),
            checkbox.leadingAnchor.constraint(equalTo: inputField.leadingAnchor),

            hoverLabel.topAnchor.constraint(equalTo: checkboxLabel.bottomAnchor, constant: 20),
            hoverLabel.leadingAnchor.constraint(equalTo: addLabel.leadingAnchor),
            hoverView.centerYAnchor.constraint(equalTo: hoverLabel.centerYAnchor),
            hoverView.leadingAnchor.constraint(equalTo: inputField.leadingAnchor),
            hoverView.widthAnchor.constraint(equalToConstant: 220),
            hoverView.heightAnchor.constraint(equalToConstant: 40),
            hoverStatusLabel.centerYAnchor.constraint(equalTo: hoverLabel.centerYAnchor),
            hoverStatusLabel.leadingAnchor.constraint(equalTo: hoverView.trailingAnchor, constant: 16),

            scrollLabel.topAnchor.constraint(equalTo: hoverLabel.bottomAnchor, constant: 20),
            scrollLabel.leadingAnchor.constraint(equalTo: addLabel.leadingAnchor),
            scrollView.centerYAnchor.constraint(equalTo: scrollLabel.centerYAnchor),
            scrollView.leadingAnchor.constraint(equalTo: inputField.leadingAnchor),
            scrollView.widthAnchor.constraint(equalToConstant: 360),
            scrollView.heightAnchor.constraint(equalToConstant: 120),

            dragLabel.topAnchor.constraint(equalTo: scrollLabel.bottomAnchor, constant: 24),
            dragLabel.leadingAnchor.constraint(equalTo: addLabel.leadingAnchor),
            dropTarget.centerYAnchor.constraint(equalTo: dragLabel.centerYAnchor),
            dropTarget.leadingAnchor.constraint(equalTo: inputField.leadingAnchor),
            dropTarget.widthAnchor.constraint(equalToConstant: 190),
            dropTarget.heightAnchor.constraint(equalToConstant: 48),
            dropTargetLabel.centerYAnchor.constraint(equalTo: dragLabel.centerYAnchor),
            dropTargetLabel.leadingAnchor.constraint(equalTo: dropTarget.trailingAnchor, constant: 12),

            delayedLabel.topAnchor.constraint(equalTo: dragLabel.bottomAnchor, constant: 24),
            delayedLabel.leadingAnchor.constraint(equalTo: addLabel.leadingAnchor),
            delayedStatusButton.centerYAnchor.constraint(equalTo: delayedLabel.centerYAnchor),
            delayedStatusButton.leadingAnchor.constraint(equalTo: inputField.leadingAnchor),
            delayedStatusLabel.centerYAnchor.constraint(equalTo: delayedLabel.centerYAnchor),
            delayedStatusLabel.leadingAnchor.constraint(equalTo: delayedStatusButton.trailingAnchor, constant: 16),

            secondWindowLabel.topAnchor.constraint(equalTo: delayedLabel.bottomAnchor, constant: 20),
            secondWindowLabel.leadingAnchor.constraint(equalTo: addLabel.leadingAnchor),
            secondWindowButton.centerYAnchor.constraint(equalTo: secondWindowLabel.centerYAnchor),
            secondWindowButton.leadingAnchor.constraint(equalTo: inputField.leadingAnchor),

            runIDLabel.topAnchor.constraint(equalTo: secondWindowLabel.bottomAnchor, constant: 18),
            runIDLabel.leadingAnchor.constraint(equalTo: addLabel.leadingAnchor),
            resetButton.centerYAnchor.constraint(equalTo: runIDLabel.centerYAnchor),
            resetButton.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -20),
        ])
    }

    private func positionDragItem() {
        guard !positionedDragItem, let root = window?.contentView else { return }
        let dropFrame = dropTarget.frame
        dragItem.frame = NSRect(x: max(0, dropFrame.minX - 148), y: dropFrame.midY - 24, width: 120, height: 48)
        positionedDragItem = true
    }

    @objc private func addPressed() {
        state.incrementSubmissions()
        updateSubmissionLabel()
        inputField.stringValue = ""
    }

    func controlTextDidChange(_ obj: Notification) {
        guard obj.object as? NSTextField === inputField else { return }
        state.noteTextEntry(inputField.stringValue)
    }

    @objc private func dropdownChanged() {
        state.selectDropdown(dropdown.titleOfSelectedItem ?? "First")
    }

    @objc private func checkboxChanged() {
        state.setCheckbox(checkbox.state == .on)
    }

    @objc private func delayedStatusPressed() {
        delayedStatusLabel.stringValue = "Delayed status: pending"
        delayedWorkItem?.cancel()
        let item = DispatchWorkItem { [weak self] in
            guard let self else { return }
            self.state.setDelayedStatus("completed")
            self.delayedStatusLabel.stringValue = "Delayed status: completed"
        }
        delayedWorkItem = item
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.2, execute: item)
    }

    @objc private func secondWindowPressed() {
        if let secondWindowController {
            secondWindowController.close()
            state.setSecondWindowOpen(false)
            self.secondWindowController = nil
            secondWindowButton.title = "Open second window"
            return
        }
        let second = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 460, height: 260),
            styleMask: [.titled, .closable],
            backing: .buffered,
            defer: false
        )
        second.title = "Computer Use Fixture — Second"
        second.setAccessibilityIdentifier("fixture-second-window")
        second.setAccessibilityTitle("Computer Use Fixture — Second")
        second.delegate = self
        let controller = NSWindowController(window: second)
        self.secondWindowController = controller
        let note = NSTextField(labelWithString: "Second fixture window. Close it to continue.")
        note.setAccessibilityIdentifier("fixture-second-note")
        note.setAccessibilityLabel("Second window note")
        note.translatesAutoresizingMaskIntoConstraints = false
        second.contentView = note
        note.centerXAnchor.constraint(equalTo: second.contentView!.centerXAnchor).isActive = true
        note.centerYAnchor.constraint(equalTo: second.contentView!.centerYAnchor).isActive = true
        second.center()
        second.makeKeyAndOrderFront(nil)
        secondWindowButton.title = "Close second window"
        state.setSecondWindowOpen(true)
    }

    @objc private func resetPressed() {
        let freshRunID = UUID().uuidString
        state.markResetRequested(newRunID: freshRunID)
        delayedWorkItem?.cancel()
        delayedWorkItem = nil
        if let secondWindowController {
            secondWindowController.close()
            self.secondWindowController = nil
            secondWindowButton.title = "Open second window"
        }
        do {
            state.markResetCompleted(newRunID: freshRunID)
            let store = try EventStore(fixtureDirectory: fixtureDirectory, runID: freshRunID)
            let newState = FixtureState(store: store, runID: freshRunID)
            state = newState
            currentRunID = freshRunID
            dropdown.selectItem(withTitle: "First")
            checkbox.state = .off
            hoverStatusLabel.stringValue = "Status: not hovered"
            delayedStatusLabel.stringValue = "Delayed status: idle"
            dropTargetLabel.stringValue = "Drop target: waiting"
            runIDLabel.stringValue = "Run: \(freshRunID)"
            updateSubmissionLabel()
            newState.snapshot()
        } catch {
            NSSound.beep()
            NSLog("ComputerUseFixture: reset failed: %@", String(describing: error))
        }
    }

    private func updateSubmissionLabel() {
        submissionCountLabel.stringValue = "Submissions: \(state.submissionCount())"
    }

    func windowDidResize(_ notification: Notification) {
        guard notification.object as? NSWindow === window else { return }
        let size = window?.frame.size ?? .zero
        state.noteWindowResized(width: size.width, height: size.height)
    }

    func windowWillClose(_ notification: Notification) {
        guard notification.object as? NSWindow === secondWindowController?.window else { return }
        secondWindowController = nil
        secondWindowButton.title = "Open second window"
        state.setSecondWindowOpen(false)
    }
}

final class FixtureAppDelegate: NSObject, NSApplicationDelegate {
    private let arguments: AppArguments
    private var windowController: FixtureWindowController?

    init(arguments: AppArguments) {
        self.arguments = arguments
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSRunningApplication.current.activate(options: [.activateIgnoringOtherApps, .activateAllWindows])
        let app = NSApplication.shared
        let menu = NSMenu()
        let appMenuItem = NSMenuItem()
        menu.addItem(appMenuItem)
        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "Quit Computer Use Fixture", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        appMenuItem.submenu = appMenu
        app.mainMenu = menu
        do {
            let store = try EventStore(fixtureDirectory: arguments.fixtureDirectory, runID: arguments.runID)
            let state = FixtureState(store: store, runID: arguments.runID)
            windowController = FixtureWindowController(fixtureDirectory: arguments.fixtureDirectory, state: state)
        } catch {
            NSLog("ComputerUseFixture: could not start: %@", String(describing: error))
            NSApp.terminate(nil)
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        return true
    }
}

private func parseArguments(from arguments: [String]) throws -> AppArguments {
    var fixtureDirectory: URL?
    var requestedRunID: String?
    var index = 0
    while index < arguments.count {
        let argument = arguments[index]
        switch argument {
        case "--fixture-dir":
            index += 1
            guard index < arguments.count else { throw FixtureError.missingFixtureDirectory }
            fixtureDirectory = URL(fileURLWithPath: arguments[index])
        case "--run-id":
            index += 1
            guard index < arguments.count else { throw FixtureError.unresolvableRunID }
            requestedRunID = arguments[index]
        default:
            break
        }
        index += 1
    }
    if fixtureDirectory == nil {
        if let environment = ProcessInfo.processInfo.environment["TIDEBREAK_CU_FIXTURE_DIR"] {
            fixtureDirectory = URL(fileURLWithPath: environment)
        }
    }
    guard let fixtureDirectory else { throw FixtureError.missingFixtureDirectory }
    let runID: String
    if let requestedRunID {
        runID = requestedRunID
    } else {
        let formatter = DateFormatter()
        formatter.dateFormat = "yyyyMMdd-HHmmss"
        let prefix = formatter.string(from: Date())
        let suffix = UUID().uuidString.prefix(8)
        runID = "\(prefix)-\(suffix)"
    }
    return AppArguments(fixtureDirectory: fixtureDirectory, runID: runID)
}

extension FixtureError {
    func logAndExit() -> Never {
        fputs("ComputerUseFixture: \(description)\n", stderr)
        exit(2)
    }
}

guard ProcessInfo.processInfo.isOperatingSystemAtLeast(requiredMacOSVersion) else {
    FixtureError.unsupportedMacOS.logAndExit()
}
do {
    let arguments = try parseArguments(from: Array(CommandLine.arguments.dropFirst()))
    let app = NSApplication.shared
    let delegate = FixtureAppDelegate(arguments: arguments)
    app.delegate = delegate
    app.setActivationPolicy(.regular)
    app.run()
} catch {
    FixtureError.missingFixtureDirectory.logAndExit()
}
