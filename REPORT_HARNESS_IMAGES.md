# Computer-use harness and image path — final review (read-only)

## Reviewed revisions

- Integration branch: `thet/computer-use-parity`
- Reviewed commit: `4ca92a1e1e46e6b8e2bf5f19bcbc3ee884e936b2`
  (`test: align browser capfile schema with declared capabilities`)
- Base compared against: `origin/main` at `c238274feee2cefe1f1f560455aef499d541a00d`
- Branch diff: 76 commits, 154 files, +31 128 / −1 403 lines

Scope followed the review brief: bundled MCP discovery and calls in Codex,
Claude Code, and OpenCode; Grok CLI screenshot-file delivery; internal-engine
browser/native tools; durable screenshot hydration; previous-fix verification.
Chrome hidden-tab execution, Chrome geometry/focus/cancellation internals,
native helper control internals, and UI geometry were intentionally not
re-reviewed.

## Verdict

No P1 or P2 findings. No confirmed transport, configuration, or API mismatch
prevents a supported harness from using computer use. Three P3 nits are
reported below for the integration owner; none block merge.

## Findings

### P3-1 — `browser screenshot --output` is not symlink-safe, atomic, or mode-enforcing on an existing file

- File/line: `crates/tidebreak-cli/src/browser.rs:2241-2265`
- Severity: P3

`write_screenshot_output` opens the destination with
`write(true).create(true).truncate(true)` and `mode(0o600)`. On Unix the mode
argument applies only when the file is created; an existing file keeps its
old permissions, and an existing symlink is followed and truncated. The
computer CLI path already solves this correctly in
`crates/tidebreak-cli/src/computer_use/mod.rs:920-950`
(`write_image_private`): refuse a destination symlink, write a
same-directory temp file with `create_new`, set 0600 explicitly, sync, then
rename into place.

Scenario: the Grok instruction text tells the agent to pick a fresh private
PNG path and the CLI writes into that path. If the agent reuses an existing
path (or a symlink) under a shared temp directory, `browser screenshot` can
write visible page pixels to a file whose permissions were not made private,
and it follows a symlink where the computer CLI would refuse.

Fix: reuse the `write_image_private` semantics for the browser screenshot
output path (or extract them into one shared helper), and add a test mirroring
`image_files_are_private_and_symlink_safe` in
`crates/tidebreak-cli/src/computer_use/tests.rs`.

### P3-2 — Grok prompt advertises browser lifecycle/diagnostics commands without capability gating

- File/line: `crates/tidebreak-harness/src/grok/session.rs:509-514`
- Severity: P3

`browser_instructions` gates `browser act` on
`browser.semantic_actions` (line 516), but the `open`/`activate`/`close`/
`diagnostics` paragraph is emitted unconditionally behind soft wording ("If
the host supports lifecycle tools..."). `BrowserChannelSpec`
(`crates/tidebreak-harness/src/lib.rs:606-642`) carries only
`semantic_actions`, so the Grok adapter has no way to know the capfile's
`lifecycle` and `developer_diagnostics` flags before composing the prompt.

Scenario: an agent on a runtime without lifecycle/diagnostics support follows
the prompt and calls `browser_open` or `browser_diagnostics`; the bridge
returns unsupported, which the prompt already tells the agent to respect, so
this is recoverable, but the advertised surface is wider than the host
declared.

Fix: carry `lifecycle` and `developer_diagnostics` on `BrowserChannelSpec`
(or have the Grok adapter read them from the capfile) and emit those command
paragraphs only when the corresponding capability is true.

### P3-3 — Development qualification commands name a nonexistent Cargo package

- File/line: `docs/computer-use.md:95-96`
- Severity: P3

The commands target `-p tidebreak-server-core`, but that crate does not exist
in this workspace; the crates are `tidebreak-server` and
`tidebreak-server-api` (verified with `ls crates`). A developer following the
documented qualification steps gets Cargo's "no matching package" error.

Fix: update the commands to `-p tidebreak-server` with the `code::chrome`
and `engine::internal::` filters respectively.

## CHECKED

### Capfile minting and injection

- `attach_and_spawn_worker` (`crates/tidebreak-server/src/code/runtime/workers.rs`)
  issues a browser channel only when the browser runtime, bridge executable,
  and workspace are all present, and a native channel only when the native
  runtime, bridge executable, and workspace are all present and the runtime is
  available.
- `SessionSpec` carries both channels; `apply_child_env_tokio`
  (`crates/tidebreak-harness/src/browser_channel.rs`) clears, restores the
  filtered snapshot, applies launch env, then injects
  `TIDEBREAK_BROWSER_CAPFILE` / `TIDEBREAK_NATIVE_CAPFILE` last.
- All four adapters (Claude, Codex, OpenCode, Grok) call the same
  `apply_child_env_tokio` path.
- Capfiles are version-1, single-purpose, 0600/0700, atomically written,
  loopback-only, and revoked on terminal session paths. The browser capfile
  schema test now asserts exactly
  `{version, endpoint, token, semantic_actions, lifecycle, developer_diagnostics}`.

### Bundled MCP discovery/calls

- Claude Code: `claude/browser.rs` merges `tb-browser` and `tb-native` stdio
  entries into one `--mcp-config` along with the existing HTTP approval
  server. Tests cover no-channel, approval-only, browser-only, and merged
  cases, and assert no capfile path or token rides in the config.
- Codex: `compose_app_server_plan` emits
  `mcp_servers.tb-browser={command=...,args=["browser-mcp"],env_vars=["TIDEBREAK_BROWSER_CAPFILE"]}`
  and the equivalent `tb-native` entry. I validated this exact shape with the
  installed `codex-cli 0.150.1`: `codex mcp get --json` parses the inline TOML
  into a stdio transport with `env_vars: ["TIDEBREAK_BROWSER_CAPFILE"]`, and
  an `app-server --stdio` thread/start test confirmed the MCP child receives
  the actual capfile value from `env_vars`.
- OpenCode: `opencode/session.rs` merges `mcp.tb-browser` /
  `mcp.tb-native` local entries into `OPENCODE_CONFIG_CONTENT`, preserving an
  existing config object, rejecting conflicts, and keeping capfile paths out
  of the JSON.
- Both MCP bridge commands (`browser-mcp`, `computer-mcp`) build their tool
  registries from the shared canonical core specs, validate arguments with the
  canonical validators, and serve text plus real MCP image content blocks.
  `tidebreak-mcp` enforces image-count, byte, media-type, content-address, and
  dimension checks before returning image blocks.

### Grok CLI screenshot-file path

- `computer_use_prompt` appends `computer list-tools`, canonical tool-name
  invocation with `--json`, and `--output <fresh-private-png-path>` guidance
  for `computer_capture_screen` and `chrome_screenshot`; it tells the agent to
  call `read_file` on the exact path.
- `computer` CLI validates known tool names, parses JSON arguments, saves the
  first result image privately and atomically via `write_image_private`, and
  prints a receipt with the path and dimensions — never base64 in stdout.
- `browser screenshot --output` similarly saves fitted PNG/JPEG bytes and
  prints a receipt; the Grok browser instructions point at this path and at
  `read_file` (P3-1 covers the writer divergence from the computer CLI).
- A captured `grok 1.0.13` image-read fixture is checked in
  (`fixtures/grok/1.0.13/image-read.{ndjson,expected.json,request.json}`) and
  the parser maps `read_file` image completion to `Image received (image/png)`
  without forwarding base64 into normalized event text.

### Internal engine tools

- `InternalSession::launch` builds native tools then browser tools from the
  same channel specs external harnesses get; a capfile the engine cannot
  trust fails the launch instead of silently dropping the surface.
- Internal browser tools gate optional tools on the capfile's
  `semantic_actions`, `lifecycle`, and `developer_diagnostics` flags and use
  canonical core schemas/validators.
- Internal native tools register the whole canonical computer session surface,
  mark control/Chrome tools sensitive, retry transport failures only with the
  same request id, and answer unknown outcomes by fetching the host's stored
  result.
- Screenshot pixels become real `ToolOutput` image blocks with the same
  1 MiB-per-image / 2 MiB-frame budgets used by the MCP bridge; base64 never
  enters model text or structured data.

### Prior fixes verified

- Real internal-engine browser client: `browser_tools.rs` reads the capfile,
  derives a loopback `/code/browser` client, uses the canonical routes, and
  is covered by capfile/toolset/screenshot tests.
- Image producer byte bounds: browser desktop producer errors above
  `MAX_BROWSER_SCREENSHOT_PNG_BYTES`; Chrome producer re-captures at smaller
  scale to the shared 1 MiB image budget and refuses if it cannot fit; the
  native adapter refuses over-budget images/frames with sizing guidance.
- Exact screenshot dimensions: Chrome returns the delivered PNG's actual
  IHDR dimensions; CLI and internal consumers read dimensions from decoded
  bytes and refuse zero/oversized/mislabeled images; the native adapter does
  not rescale, so reported dimensions always describe delivered pixels.
- Additive `Images` preview/retention/transcript hydration:
  `ToolResultPreview::Images` is projected for `browser_screenshot`,
  `computer_capture_screen`, and `chrome_screenshot`; blob-liveness,
  image-attachment API, transcript rebuild, UI parsers, and wire types all
  handle it; `image_hydration.rs` proves bytes survive, retain order, and
  rehydrate into later model requests.
- Native HTTP-disconnect operation ownership: the `/code/native/execute`
  route spawns the runtime call so host execution outlives the handler; the
  stored result is fetchable via `/code/native/result`, and the route test
  proves early disconnect does not drop the operation.
- Mark renumbering regression: `finish_tree_read` no longer replaces the
  capture mark table, and `a_narrow_tree_read_preserves_the_last_screenshot_mark_mapping`
  pins the behavior.
- Chrome result limits shared with the session channel: `bound_chrome_result`
  enforces the same 1 MiB image / 2 MiB frame budgets and the shared
  `ChromeJournal` preserves request identity after large results expire.

## Commands and output

Repository identity:

```text
$ git rev-parse HEAD
4ca92a1e1e46e6b8e2bf5f19bcbc3ee884e936b2
$ git rev-parse origin/main
c238274feee2cefe1f1f560455aef499d541a00d
$ git diff --stat origin/main...HEAD | tail -1
 154 files changed, 31128 insertions(+), 1403 deletions(-)
```

Codex production MCP config shape (installed `codex-cli 0.150.1`):

```text
$ codex -c 'mcp_servers.tb-browser={command="/bin/true",args=["browser-mcp"],env_vars=["TIDEBREAK_BROWSER_CAPFILE"]}' \
    mcp get tb --json
{
  "name": "tb",
  "enabled": true,
  "transport": {
    "type": "stdio",
    "command": "/bin/true",
    "args": ["browser-mcp"],
    "env": null,
    "env_vars": ["TIDEBREAK_BROWSER_CAPFILE"],
    "cwd": null
  },
  ...
}
```

Codex `env_vars` propagation (thread/start with a Python MCP probe):

```text
---ENVFILE---
/tmp/example-cap.json
---STDOUT mcp---
{"method":"mcpServer/startupStatus/updated","params":{"...","name":"tb-browser","status":"starting",...}}
```

The probe MCP process wrote the value of `TIDEBREAK_BROWSER_CAPFILE` it
received; the file contained `/tmp/example-cap.json`, proving the exact env
name listed in `env_vars` reaches the bridge child.

Committed installed-harness MCP image qualification
(`scripts/fixtures/harness-mcp-image-qualification.json`, dated 2026-09-08):

```text
codex-cli 0.153.2   passed  matching request.input[6].output[2] sha256=2d10c869...
Claude Code 2.1.263 passed  matching request.messages[2].content[0].content[1]
opencode 1.18.23    passed  matching request.input[3].output[1]
```

Cargo test attempt in this sandbox:

```text
$ cargo test -p tidebreak-harness --lib grok::
error: no matching package named `image` found
location searched: crates.io index
required by package `tidebreak-cli v0.0.0 (/workspace/tidebreak/crates/tidebreak-cli)`
note: offline mode (via `--offline`) can sometimes cause surprising resolution failures
```

## Unverified limits

- No Rust unit tests could be executed here: the offline Cargo cache does not
  include the newly pinned `image = 0.25.10` crate required by
  `tidebreak-cli`. Findings are from code reading plus the empirical Codex
  and MCP probe checks above.
- The Mac is locked and was not available. No native/live desktop
  qualification is claimed, including packaged helper, real grants, real
  screenshot bytes through every harness, and native input behavior.
- The checked-in MCP image qualification fixture still lists
  `actual_tidebreak_bridge`, `native_or_browser_capture`, and `model_reasoning`
  as remaining gates. This review covers the bridge configuration and
  transport statically (and Codex env propagation empirically), but not a
  live capture on this machine.
- Installed Linux harness versions here differ from the Mac qualification
  artifact (Codex 0.150.1 vs 0.153.2, Claude 2.1.247 vs 2.1.263, Grok 1.0.5 vs
  1.0.13). A local full Codex transport fixture run reached MCP discovery but
  not invocation before the harness timeout; this was treated as an
  environment/version mismatch, not a branch finding.
- Chrome hidden-tab execution, Chrome geometry/focus/cancellation internals,
  native helper control internals, and UI geometry were not reviewed (per
  brief).

No production source was changed; no PR was opened and no GitHub comments or
reviews were posted.
