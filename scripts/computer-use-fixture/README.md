# Computer-use native acceptance fixture

A deliberately small AppKit application that exists so Tidebreak's native
computer-use integration can be proven against real macOS accessibility events,
not against tool-success strings. The fixture window exposes every interaction
the integration contract needs and writes bounded JSON records to a caller-owned
directory as the events actually happen.

This fixture is acceptance tooling, not product code. It has no network access
beyond macOS's loopback interfaces, no secrets, no shell execution, no access
to host files, and no approval surface.

## App identity and behavior

- Bundle id: `dev.tidebreak.ComputerUseFixture` (stable, deliberately not a
Tidebreak production bundle id)
- Window title: `Computer Use Fixture`
- The window stays at a typical desktop scale (`920 × 760`, expandable) and
contains one text field, one `Add` button, one titled pop-up/dropdown, one
checkbox, one hover status area, one draggable item with a drop target, one
bounded scroll area, one delayed status transition, and a visible prompt for a
second session window.

Every UI event is written as one atomic JSON object to
`<fixture-dir>/events/<run-id>/<sequence>.json`. Submitting the text field via
the `Add` button increments the exact submission count; resetting the fixture
increments the reset count and clears the session. The fixture never infers
that an interaction happened from a tool success string.

## Build and run

Requires macOS with Xcode Command Line Tools (Swift, `clang`/`swiftc`, and
`osacompile` are used). macOS 14 or newer is recommended; the pop-up menu APIs
below require macOS 13.

Build into a caller-specified ignored temp directory:

```sh
scripts/computer-use-fixture/build.sh /tmp/tidebreak-cu-fixture
open /tmp/tidebreak-cu-fixture/ComputerUseFixture.app
```

The script creates `<directory>/ComputerUseFixture.app`, writes a marker file
`<directory>/BUILD.repro.md` containing the reproducible build inputs and
commands, and prints the exact build/run command. If the fixture directory
already exists, the script rebuilds only when inputs have changed.

## Interaction and evidence format

Run with an explicit fixture directory (or a directory inside `TMPDIR`):

```sh
open /tmp/tidebreak-cu-fixture/ComputerUseFixture.app --args --fixture-dir /tmp/tidebreak-cu-fixture/state
```

The fixture directory may also be provided as
`TIDEBREAK_CU_FIXTURE_DIR`. Each launch creates a fresh run id
(`YYYYMMDD-HHMMSS-uuid-8`), and every event for that run is written under
`events/<run-id>/<zero-padded-sequence>.json`. Each event JSON has this shape:

```json
{
  "event": "submission",
  "run_id": "…",
  "sequence": 7,
  "payload": {"slot": 1}
}
```

Events: `submission`, `dropdown_selection`, `checkbox_toggle`, `hover_status`,
`drag_started`, `drag_dropped`, `delayed_status`, `window_resized`,
`second_window_opened`, `second_window_closed`, `reset_requested`,
`reset_completed`, `launch_ready`, `state_snapshot`. Reusing a sequence number
or writing an event that did not happen are acceptance failures, so re-running
the smoke on the same fixture directory is not a shortcut.

## Evidence distinction

The smoke runner reads fresh targets, invokes the helper only through the
bounded adapter, and verifies the fixture's own state records after each real
UI action. It does not claim a pass from compile, from a helper success string,
or from a screenshot that happens to exist.
