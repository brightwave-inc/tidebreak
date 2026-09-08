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
the `Add` button records the submitted value and increments the count. Reset
clears the controls and starts a fresh run. The fixture never infers
that an interaction happened from a tool success string.

## Build and run

Requires macOS with Xcode Command Line Tools (`swiftc` and `codesign`). macOS
14 or newer is recommended; the pop-up menu APIs below require macOS 13.

Build into a caller-specified ignored temp directory:

```sh
scripts/computer-use-fixture/build.sh /tmp/tidebreak-cu-fixture
```

The script creates `/tmp/tidebreak-cu-fixture/ComputerUseFixture.app`, writes
`/tmp/tidebreak-cu-fixture/BUILD.repro.md` (the exact source hashes and build
commands), and prints the run command. If the directory already exists, the
script rebuilds only when the sources changed. Verify the identity before
launching:

```sh
/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' \
  /tmp/tidebreak-cu-fixture/ComputerUseFixture.app/Contents/Info.plist
# dev.tidebreak.ComputerUseFixture
```

## Interaction and evidence format

Run with an explicit fixture directory (or a directory inside `TMPDIR`):

```sh
open /tmp/tidebreak-cu-fixture/ComputerUseFixture.app --args --fixture-dir /tmp/tidebreak-cu-fixture/state
```

The fixture directory may also be provided as
`TIDEBREAK_CU_FIXTURE_DIR`, and `--run-id` pins a fresh id. Each launch creates
a fresh run id (`YYYYMMDD-HHMMSS-uuid-8`), and every event for that run is
written under
`events/<run-id>/<zero-padded-sequence>.json`. Reset keeps writing to the
current run until the reset completes, then writes a final `state_snapshot`
with the new run id into that same current run as a transition record; the new
run's events live under `events/<new-run-id>/`. Each event JSON has this shape:

```json
{
  "event": "submission",
  "run_id": "…",
  "sequence": 7,
  "payload": {"count": 1, "value": "Native acceptance example"}
}
```

Events: `launch_ready`, `submission`, `text_entry`, `dropdown_selection`,
`checkbox_toggle`, `hover_status`, `drag_started`, `drag_dropped`, `scroll`,
`delayed_status`, `window_resized`,
`second_window_opened`, `second_window_closed`, `reset_requested`,
`reset_completed`, `state_snapshot`. Reusing a sequence number or writing an
event that did not happen are acceptance failures, so re-running the smoke on
the same fixture directory is not a shortcut.

## Smoke verification

Build the fixture, launch it with a fresh run id, then run the shipped smoke
from macOS with the bundled Tidebreak computer CLI:

```sh
fixture_dir=$(mktemp -d)/tidebreak-cu-fixture
scripts/computer-use-fixture/build.sh "$fixture_dir"
run_id="smoke-$(uuidgen)"

open "$fixture_dir/ComputerUseFixture.app" --args \
  --fixture-dir "$fixture_dir/state" --run-id "$run_id"

node scripts/computer-use-native-smoke.mjs \
  --cli /Applications/Tidebreak.app/Contents/MacOS/tidebreak \
  --fixture-dir "$fixture_dir/state" \
  --run-id "$run_id" \
  --app-path "$fixture_dir/ComputerUseFixture.app"
```

The runner explicitly requests foreground mode for launch, focus, clicks, typing,
keys, hover, drag, scroll, and resize. Review the normal per-app control and
foreground prompts before approving each scope. A refusal, Stop, or
`requires_foreground` result ends the run without retrying. Reads and captures
keep their separate grants. This smoke does not qualify background focus or
pointer retention.

The runner uses `computer <tool> --json '<arguments>'`. For capture, it adds
`--output` and checks the returned `image_file`, fresh PNG bytes, and metadata.
Set `TIDEBREAK_NATIVE_CAPFILE` to the capability file issued by the genuine
Code session before running the command. Do not create a replacement capability.

The runner never infers a pass from compile, from a helper success string, or
from a screenshot that happens to exist. Each act is followed by the fixture's
own atomic JSON evidence: exactly one submission record, a dropdown and
checkbox transition, hover and drag state, scroll offset, delayed transition,
and window geometry after resize. The runner checks every native result for
failure, waits for the actual text and scroll changes, and requires the before
and after screenshots to differ. Screenshots are saved as PNG files under
`<fixture-dir>/screenshots/<run-id>/` with a JSON metadata sidecar. Stop,
takeover, approval surfaces, concurrent ownership, and uncertain-outcome
recovery require live parent actions and are reported as separate remaining
gates rather than claimed from the script.


Run the runner tests and macOS type check before hardware acceptance:

```sh
node --test scripts/computer-use-native-smoke.test.mjs
scripts/computer-use-fixture/validate.sh
```

On macOS, the test suite compiles the same `EventStore.swift` that the app uses.
It writes real records and checks the runner's event schema, reset ownership,
and refusal to reuse an existing run. Fake native calls test the runner's
failure handling; those tests do not qualify hardware control.
