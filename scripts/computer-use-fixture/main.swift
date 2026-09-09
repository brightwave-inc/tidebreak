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

struct AppArguments {
    var fixtureDirectory: URL
    var runID: String
    var background: Bool
    var recordInput: Bool
}

// Bounded, inspectable fixture state. All state is intentionally observable
// through the accessibility tree, so a native helper can read it back.
final class FixtureState {
    let runID: String
    private let store: EventStore
    private var recordedInputCount = 0
    private var independentPointerMoves = 0
    private var submissionCounter = 0
    private var textValue = ""
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

    init(store: EventStore, runID: String, background: Bool = false, recordInput: Bool = false) {
        self.store = store
        self.runID = runID
        store.write("launch_ready", payload: ["app_id": fixtureBundleID, "title": fixtureWindowTitle, "background": background, "record_input": recordInput])
    }

    func submissionCount() -> Int { submissionCounter }
    func selectedDropdown() -> String { dropdownSelection }
    func isCheckboxChecked() -> Bool { checkboxChecked }
    func isHovered() -> Bool { hovered }
    func wasDragDropped() -> Bool { dragDropped }
    func dragTarget() -> String { lastDragTarget }
    func currentDelayedStatus() -> String { delayedStatus }
    func isSecondWindowOpen() -> Bool { secondWindowOpen }

    /// Observe delivered input without changing how AppKit dispatches it.
    func noteInput(_ event: NSEvent) {
        guard recordedInputCount < 400 else { return }
        recordedInputCount += 1
        let local = event.locationInWindow
        let point = event.cgEvent?.location ?? .zero
        var payload: [String: Any] = [
            "type": event.type.rawValue,
            "window_number": event.windowNumber,
            "location_in_window": ["x": local.x, "y": local.y],
            "quartz_location": ["x": point.x, "y": point.y],
            "app_active": NSApp.isActive,
            "window_key": event.window?.isKeyWindow ?? false,
            "source_pid": event.cgEvent?.getIntegerValueField(.eventSourceUnixProcessID) ?? -1,
        ]
        if event.type == .keyDown || event.type == .keyUp {
            payload["key_code"] = event.keyCode
        }
        if [.leftMouseDown, .leftMouseUp, .rightMouseDown, .rightMouseUp].contains(event.type) {
            payload["click_count"] = event.clickCount
        }
        store.write("input_received", payload: payload)
        if event.type == .mouseMoved,
           (event.cgEvent?.getIntegerValueField(.eventSourceUnixProcessID) ?? 0) > 0,
           !NSApp.isActive {
            independentPointerMoves += 1
            store.write("independent_mouse_moved", payload: payload)
            snapshot()
        }
    }

    func snapshot() {
        store.write("state_snapshot", payload: snapshotPayload())
    }

    private func snapshotPayload() -> [String: Any] {
        [
            "app_id": fixtureBundleID,
            "title": fixtureWindowTitle,
            "run_id": runID,
            "submission_count": submissionCounter,
            "text_value": textValue,
            "dropdown": dropdownSelection,
            "checkbox": checkboxChecked,
            "hovered": hovered,
            "independent_pointer_moves": independentPointerMoves,
            "drag_dropped": dragDropped,
            "drag_target": lastDragTarget,
            "delayed_status": delayedStatus,
            "second_window_open": secondWindowOpen,
            "scroll_offset": Int(scrollOffset),
            "window_size": ["width": Int(windowWidth), "height": Int(windowHeight)],
        ]
    }

    func incrementSubmissions(text: String) {
        submissionCounter += 1
        textValue = ""
        store.write("submission", payload: ["count": submissionCounter, "value": text])
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
        textValue = value
        store.write("text_entry", payload: ["value": value])
        snapshot()
    }

    func markResetRequested(newRunID: String) {
        store.write("reset_requested", payload: ["new_run_id": newRunID])
    }

    /// The old run's final snapshot describes the freshly reset run so the
    /// accepting runner can observe the transition from the directory it
    /// already owns. Events after this point belong to the new run.
    func markResetCompleted(newState: FixtureState) {
        store.write("reset_completed", payload: ["new_run_id": newState.runID])
        store.write("state_snapshot", payload: newState.snapshotPayload())
    }
}

class FlippedView: NSView {
    override var isFlipped: Bool { true }
}

final class HoverStatusView: NSView {
    var onHoverChanged: ((Bool) -> Void)?
    var tracksWhileInactive = false
    private var trackingArea: NSTrackingArea?

    override func updateTrackingAreas() {
        if let trackingArea { removeTrackingArea(trackingArea) }
        let activation: NSTrackingArea.Options = tracksWhileInactive ? .activeAlways : .activeInKeyWindow
        let options: NSTrackingArea.Options = [activation, .inVisibleRect, .mouseEnteredAndExited]
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
        onDragStarted?(accessibilityIdentifier())
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
        if let target = root.subviews.first(where: {
            $0.accessibilityIdentifier() == "fixture-drop-target" && $0.frame.intersects(frame)
        }) {
            onDrop?(target.accessibilityIdentifier())
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
    private let background: Bool
    private let recordInput: Bool
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

    init(fixtureDirectory: URL, state: FixtureState, background: Bool = false, recordInput: Bool = false) {
        self.fixtureDirectory = fixtureDirectory
        self.background = background
        self.recordInput = recordInput
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
        window.acceptsMouseMovedEvents = true
        hoverView.tracksWhileInactive = background
        buildContent()
        window.center()
        if background {
            window.orderFront(nil)
            window.makeFirstResponder(inputField)
        } else {
            window.makeKeyAndOrderFront(nil)
        }
        window.contentView?.layoutSubtreeIfNeeded()
        positionDragItem()
        state.snapshot()
    }

    func noteInput(_ event: NSEvent) {
        guard event.window === window else { return }
        state.noteInput(event)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("unavailable") }

    private func buildContent() {
        guard let window else { return }
        let root = FlippedView()
        root.setAccessibilityIdentifier("fixture-root")
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
        hoverView.setAccessibilityElement(true)
        hoverView.setAccessibilityRole(.group)
        hoverView.setAccessibilityIdentifier("fixture-hover-area")
        hoverView.setAccessibilityLabel("Hover status area")
        hoverView.setAccessibilityHelp("Hover enters a mouse tracking area and switches the status")
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
        scrollView.contentView.postsBoundsChangedNotifications = true
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
        dragItem.setAccessibilityElement(true)
        dragItem.setAccessibilityRole(.group)
        dragItem.setAccessibilityIdentifier("fixture-drag-item")
        dragItem.setAccessibilityLabel("Draggable item")
        dragItem.setAccessibilityHelp("Mouse-drag this item onto the drop target")
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

        dropTarget.setAccessibilityElement(true)
        dropTarget.setAccessibilityRole(.group)
        dropTarget.setAccessibilityIdentifier("fixture-drop-target")
        dropTarget.setAccessibilityLabel("Drop target")
        dropTarget.setAccessibilityHelp("Drop the draggable item here")
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

            scrollLabel.topAnchor.constraint(equalTo: hoverView.bottomAnchor, constant: 20),
            scrollLabel.leadingAnchor.constraint(equalTo: addLabel.leadingAnchor),
            scrollView.topAnchor.constraint(equalTo: scrollLabel.topAnchor),
            scrollView.leadingAnchor.constraint(equalTo: inputField.leadingAnchor),
            scrollView.widthAnchor.constraint(equalToConstant: 360),
            scrollView.heightAnchor.constraint(equalToConstant: 120),

            dragLabel.topAnchor.constraint(equalTo: scrollView.bottomAnchor, constant: 30),
            dragLabel.leadingAnchor.constraint(equalTo: addLabel.leadingAnchor),
            dropTarget.centerYAnchor.constraint(equalTo: dragLabel.centerYAnchor),
            dropTarget.leadingAnchor.constraint(equalTo: inputField.leadingAnchor, constant: 148),
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
        guard !positionedDragItem, window?.contentView != nil else { return }
        let dropFrame = dropTarget.frame
        dragItem.frame = NSRect(x: max(0, dropFrame.minX - 148), y: dropFrame.midY - 24, width: 120, height: 48)
        positionedDragItem = true
    }

    @objc private func addPressed() {
        state.incrementSubmissions(text: inputField.stringValue)
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
        state.setDelayedStatus("pending")
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
        let secondRoot = FlippedView()
        second.contentView = secondRoot
        let note = NSTextField(labelWithString: "Second fixture window. Close it to continue.")
        note.translatesAutoresizingMaskIntoConstraints = false
        note.setAccessibilityIdentifier("fixture-second-note")
        note.setAccessibilityLabel("Second window note")
        secondRoot.addSubview(note)
        NSLayoutConstraint.activate([
            note.centerXAnchor.constraint(equalTo: secondRoot.centerXAnchor),
            note.centerYAnchor.constraint(equalTo: secondRoot.centerYAnchor),
        ])
        second.center()
        if background {
            second.orderFront(nil)
        } else {
            second.makeKeyAndOrderFront(nil)
        }
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
            let store = try EventStore(fixtureDirectory: fixtureDirectory, runID: freshRunID)
            let newState = FixtureState(store: store, runID: freshRunID, background: background, recordInput: recordInput)
            let oldState = state
            state = newState
            currentRunID = freshRunID
            inputField.stringValue = ""
            scrollView.contentView.scroll(to: .zero)
            scrollView.reflectScrolledClipView(scrollView.contentView)
            lastScrollY = 0
            positionedDragItem = false
            positionDragItem()
            dropdown.selectItem(withTitle: "First")
            checkbox.state = .off
            hoverStatusLabel.stringValue = "Status: not hovered"
            delayedStatusLabel.stringValue = "Delayed status: idle"
            dropTargetLabel.stringValue = "Drop target: waiting"
            runIDLabel.stringValue = "Run: \(freshRunID)"
            updateSubmissionLabel()
            let size = window?.frame.size ?? .zero
            newState.noteWindowResized(width: size.width, height: size.height)
            oldState.markResetCompleted(newState: newState)
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
    private var inputMonitor: Any?

    init(arguments: AppArguments) {
        self.arguments = arguments
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        if !arguments.background {
            NSRunningApplication.current.activate(options: [.activateAllWindows])
        }
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
            let state = FixtureState(store: store, runID: arguments.runID, background: arguments.background, recordInput: arguments.recordInput)
            windowController = FixtureWindowController(
                fixtureDirectory: arguments.fixtureDirectory, state: state,
                background: arguments.background, recordInput: arguments.recordInput)
            if arguments.recordInput {
                inputMonitor = NSEvent.addLocalMonitorForEvents(matching: [
                    .keyDown, .keyUp, .leftMouseDown, .leftMouseUp,
                    .leftMouseDragged, .mouseMoved, .rightMouseDown, .rightMouseUp,
                ]) { [weak self] event in
                    self?.windowController?.noteInput(event)
                    return event
                }
            }
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
    var background = ProcessInfo.processInfo.environment["TIDEBREAK_CU_FIXTURE_BACKGROUND"] == "1"
    var recordInput = ProcessInfo.processInfo.environment["TIDEBREAK_CU_FIXTURE_RECORD_INPUT"] == "1"
    var index = 0
    while index < arguments.count {
        let argument = arguments[index]
        switch argument {
        case "--background":
            background = true
        case "--record-input":
            recordInput = true
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
    return AppArguments(fixtureDirectory: fixtureDirectory, runID: runID, background: background, recordInput: recordInput)
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
} catch let error as FixtureError {
    error.logAndExit()
} catch {
    FixtureError.missingFixtureDirectory.logAndExit()
}
