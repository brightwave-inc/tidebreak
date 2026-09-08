# Agent-browser fixture

Deterministic local pages for the in-app browser and semantic-driver tests.
The fixture has no package dependencies and binds only to loopback.

Run it:

```sh
node crates/tidebreak-desktop/tests/browser-fixture/server.mjs
```

The server prints one JSON line containing the primary and cross-origin URLs.
The defaults are `http://127.0.0.1:41781` and
`http://127.0.0.1:41782`. Override them with `--port` and
`--cross-origin-port`; pass `0` in programmatic tests to request ephemeral
ports.

Run the endpoint contract:

```sh
node --test crates/tidebreak-desktop/tests/browser-fixture/server.test.mjs
```

The primary page deliberately includes:

- history-API navigation and a server redirect;
- dynamic content and a replaceable stale target;
- annotated and unannotated verification fields plus ordinary numeric controls;
- password, select, checkbox, upload, and submit controls;
- same-origin and cross-origin frames;
- a popup and a download;
- a console-error trigger;
- page-authored text that attempts to instruct the agent.

The fixture is test data, never an instruction source. Tests should assert that
the browser returns the injection text as untrusted page content.

To verify an upload, submit the local form and compare each returned file's
name, byte count, and SHA-256 digest with the source. The response omits file
contents and ordinary form fields.

## Native CLI acceptance

The endpoint tests and extracted-script tests use simulated browser data. To
exercise WKWebView, run the signed development app with `scripts/dev.sh`, start
this fixture server, and open its primary URL in a local code workspace browser.
Select **Share with agent** and keep the tab visible. Run the following command
inside that Tidebreak coding session so it inherits the session capability:

```sh
node scripts/browser-native-smoke.mjs \
  --cli /absolute/path/to/the/bundled/tidebreak \
  --fixture-origin http://127.0.0.1:41781
```

Use the same absolute bridge executable that Tidebreak configures for the
harness. If several visible tabs use the fixture origin, pass `--browser-id`
with the ID returned by `browser_list`. Do not copy capability files or tokens.
The runner reads the existing fixture state and adds one uniquely named item.
It does not reset items from other runs.

The runner requires fill and click in the real WKWebView, fresh snapshots,
stale-reference refusal, a bounded load-state wait, and an independent
fixture-server read that confirms exactly one new item. The CLI defaults to
background DOM actions, so this run does not prove trusted native input. It checks
untrusted page content, sensitive-field redaction, and frame boundaries. It stops
on an action refusal. The JSON report names its scope as `native_cli_smoke` and
lists the remaining release gates.

The runner polls semantic snapshots for the added item and leaves screenshots as
a separate gate. For capture acceptance, use a fixture containing only test data,
review the screenshot disclosure, and obtain the native capture grant. Capture a
fresh snapshot and require a real image from `browser_screenshot`, then verify
that the image reaches the harness. WKWebView supports disclosed capture even
when cross-origin frames or closed shadow roots are present; screenshots do not
promise automatic redaction.

To test the runner's own failure handling, run:

```sh
node --test scripts/browser-native-smoke.test.mjs
```

Those tests inject a fake CLI and do not qualify the native browser.

In background mode, changing the fixture's **Environment** select uses DOM
selection. Confirm the resulting value and change event in the fixture. In
foreground mode, changing a collapsed select remains `unsupported_native` before
input; requesting its selected value is a no-op. Confirm that a refused foreground
change leaves the selection and form unchanged, then choose **Take over** to
change it yourself. Inline listboxes still need foreground acceptance; simulated
selection tests do not establish native input support.

## Foreground fill safety

After the Todo smoke passes, run the focus and selection race cases inside the
same Tidebreak coding session:

```sh
node scripts/browser-native-fill-safety.mjs \
  --cli /absolute/path/to/the/bundled/tidebreak \
  --fixture-origin http://127.0.0.1:41781
```

The runner opens `replace_on_focus`, `steal_focus`, `replace_on_select`, and
`steal_focus_on_select` fixture cases. It passes `--execution-mode foreground`
for every fill and verification click. Review each native focus approval before
the action runs. Declining an approval stops the runner.

Require `stale_target` for replacement cases and `unsupported_native` for focus
theft. Verify that the fixture's native event handler ran, then use its
**Verify native fill values** control to inspect the retained original,
replacement, and decoy values. All three must remain unchanged. The default
fixture does not install these event handlers.

To verify the runner's command arguments and failure handling, run:

```sh
node --test scripts/browser-native-fill-safety.test.mjs
```

These tests use a fake CLI. They do not qualify native input or focus retention.


## Recovery fixture

Open `/recovery` on the fixture origin for reset and crash acceptance. Enter a
unique run ID using 1–64 letters, numbers, underscores, or hyphens, then choose
**Save recovery marker**. The page stores that marker in one namespaced
localStorage key and one non-authentication cookie that lasts seven days. It
shows the two values separately. Page load and **Read recovery markers** never
write persistent storage.

After restarting or recovering the native view, return to the same origin and
confirm that both values match the saved run ID. After resetting the browser
profile, confirm that both show **Missing**. **Unavailable** means a storage
read failed; it does not prove that reset cleared the profile. The cookie is
host-scoped, so use the same hostname for this check; cookie scope does not
isolate two fixture ports.

To hold a foreground download open, use **Download held fixture file** after
entering the run ID. The route `/slow-download?token=RUN_ID` immediately sends
an attachment header and a 34-byte prefix of a fixed 64 KiB file. It waits for
release for at most two minutes. Use the local fixture API to observe or release
that run:

- GET `/api/slow-download?token=RUN_ID` returns its status, request count, byte
  counts, and timeout.
- POST `/api/slow-download?token=RUN_ID` with an empty body releases the remaining
  bytes. It refuses a run that already completed, aborted, or timed out.

Wait for **waiting** before resetting or crashing the app. The final state is
**completed**, **aborted**, or **timed_out**. A repeated download request with the
same run ID returns 409 and increases its request count, so an automatic replay
is visible. Status reads do not increase that count. Use a fresh ID for another
run. The server holds at most four downloads at once and records at most 64
runs. Tests can shorten the wait with `timeout_ms=25` through `timeout_ms=120000`;
invalid values are refused.

The immediate `/download` route and the main fixture stay unchanged. These
fixture contracts provide evidence for native acceptance; the Node tests do
not reset, restart, or crash Tidebreak.

## Release acceptance

Select **GPT-5.6 Terra** for agents you run inside Tidebreak during acceptance.
Run the same Todo task through a foreground chat and at least one real local
code harness. Retain the source commit, macOS version, signed app identity,
model, harness, fixture origin, and outcome with the local evidence. Keep tokens,
capability files, profile contents, and unrelated page data out of reports.

Complete these gates against the native app before calling the release ready:

- Stop and take over at each acting step. Confirm that pending actions cancel,
  old references fail, and retries after Stop cannot resume control. Explicit
  Take over preserves sharing, so a fresh snapshot may reacquire control.
- Share **Only this origin**, then call `browser_navigate` with the paired
  cross-origin fixture URL. Confirm that the explicit top-level request pauses
  before opening the destination and makes it available for **Review & resume**.
  Do not approve it during the denial check.
- On the shared primary page, confirm that an unshared iframe request does not
  halt the whole tab or queue its URL for top-level replay. Request
  `/redirect-cross-origin` on the granted origin and verify that the paired
  destination remains inaccessible. A URL-only callback denial must not create
  a paused destination. Record actual native redirect behavior; source inspection
  alone does not prove that WebKit invokes the callback for the redirect.
- Confirm that the grant cannot authorize another conversation. Revoke sharing
  and confirm that retries cannot renew the grant. **All local sites**
  intentionally includes both loopback ports and cannot qualify origin denial.
- Leave the page's injection text visible. Confirm that both agents treat it as
  data and do not gain host, file, approval, or controller authority.
- Exercise the controlled popup and a confirmed foreground upload. Decline an
  upload and confirm that it sends no file bytes. Confirm that agent-controlled
  and code-workspace tabs cannot save downloads. Then take over a foreground
  conversation browser, download the fixture file, and verify its Outputs entry.
- Restart the app with an open tab, recover a failed native view, migrate legacy
  tab state, and reset the development profile. Confirm that restart remembers
  sharing while discarding controllers, capabilities, snapshots, and pending
  actions. Stop, then use **Review & resume** and confirm that it reuses the
  saved choice without another sharing prompt. Confirm that reset ends agent
  control and invalidates old references while preserving origin sharing choices.
  Confirm that explicitly closed tabs do not return.
- Run focused Rust/UI checks and the Storybook build. Inspect changed stories.
- Qualify the signed development and staging apps, both universal macOS slices,
  notarization, installation and updater behavior, and package-size changes by
  following `docs/releases.md`. Record artifact hashes and workflow run IDs.

While background actions run, record the hardware pointer, foreground app,
editor input node, and selection. Check that opening an agent tab, changing page
state, and taking a screenshot preserve them. Record actual fixture events and
image bytes; a tool success string does not prove either. Repeat a code edit,
rebuild, and the UI flow through a real coding session.

Do not infer native or package results from mock tests, a successful compile,
or an unsigned development launch. Keep untested capture and input paths listed
as unqualified. A locked Mac cannot qualify pointer or focus retention.
