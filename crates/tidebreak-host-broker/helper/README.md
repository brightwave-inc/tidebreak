# Native computer-use helper

The host broker sends one JSON request per helper process. The broker owns app grants and consent. The helper executes the authorized operation and returns JSON.

## Execution modes

Control requests accept `execution_mode: "background" | "foreground"`. Omit the field to use background mode. Successful control results include the mode used.

Background mode uses Accessibility actions. It never activates an app or posts system mouse and keyboard events. The supported operations depend on the app's Accessibility implementation:

- Single left clicks use `AXPress`. Coordinates must resolve to an app-owned Accessibility element with that action. Native menus require foreground mode.
- Text entry requires an element that accepts `AXValue`. The helper reads the value back before reporting success.
- Scroll requests use a scrollbar action or a writable normalized scrollbar value. They move one Accessibility step in the requested direction. The app determines the distance; background mode does not promise pixel-exact scrolling.
- Window resize uses Accessibility and verifies the resulting dimensions.
- Reads, captures, and waits require no foreground input. Launch requests leave a running app in the background and request nonactivating launch for a stopped app.

Hover, drag, keyboard chords, focus changes, and unsupported Accessibility targets return `requires_foreground`. The helper does not retry them with global input. The broker must obtain foreground consent before resending with `execution_mode: "foreground"`.

Foreground mode activates the granted app and uses the guarded system-input path. It can move the pointer and change keyboard focus. Cancellation releases held buttons and keys.

Accessibility access does not create an isolated macOS desktop. An app can present or activate its own UI in response to an action. Full isolation across arbitrary native apps requires a separate desktop or virtual machine. Public `CGEvent.postToPid` did not produce the required effects in an inactive AppKit fixture and is not used as an implicit fallback.

## Validation

Run `./test.sh` for no-input Swift regressions. The script works with Xcode Command Line Tools and does not require XCTest.

Native acceptance also uses `scripts/computer-use-fixture`. To qualify background mode, keep a separate harmless app frontmost and verify the pointer and foreground app throughout the run. Assert the fixture's recorded effects and capture actual pixels. A successful tool response alone does not prove that an inactive app accepted an action.
