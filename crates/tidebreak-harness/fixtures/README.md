# Harness fixtures

CI replays checked-in protocol streams without starting an engine. These tests
prove how the adapter encodes or parses the recorded shapes. They do not prove
that every installed pin still emits those shapes, or that a live model behaves
as a recorded response suggests.

## Version coverage

`src/pin.rs` chooses exact install versions. Fixture directories record the
versions used for each protocol baseline; a pin bump does not rename or refresh
a capture.

| Engine | Install pin | Checked-in coverage |
| --- | --- | --- |
| Claude Code | 2.1.259 | 2.1.233 print/stream-json baseline and MCP prompt-tool approvals. The manifest also records process observations on 2.1.238 and steering observations on 2.1.239. No complete 2.1.259 capture. |
| Codex | 0.153.4 | 0.147.0 app-server baseline; 0.153.0 MCP tool-approval elicitation. No complete 0.153.4 capture. |
| opencode | 1.18.27 | 1.18.18 HTTP/SSE baseline. No complete 1.18.27 capture. |
| Grok | 1.0.13 | 1.0.4 print-stream baseline, 1.0.5 subagent projection, and 1.0.13 ACP approvals/cancel/resume plus tool-image transport. The 1.0.13 capture uses a scripted local provider, not a live model. |

Keep this table and the pin comments accurate when changing an install version.
Capture a changed protocol at its observed version. Do not copy an older stream
into a new version directory or describe a partial capture as complete coverage.

## Fixture contents

Each scenario lives under `<harness>/<version>/` with:

- `<scenario>.ndjson`: recorded protocol frames, one JSON object per line.
- `<scenario>.expected.json`: the normalized `HarnessEvent` sequence for parser replays.
- `manifest.toml`: observed version, scenarios, available capture details, redactions, and limits.

Request payloads can have separate JSON files. ACP tests assert their request,
permission, and completion contracts directly rather than using normalized
expected files. Manifests identify reconstructed or synthetic scenarios; those
prove adapter regressions, not an additional engine observation.

The Claude `image-input` stream predates its replay test. It reports 2.1.233, but
its original command, capture date, and image bytes were not recorded. Its
manifest preserves that limit. The user frame lives in `image-input.request.json`
so the response parser does not treat an outgoing prompt as a received steer.
`fixture_replay_image_input` compares the real
request encoder with the redacted input shape and checks the full normalized
response, visible answer, session handle, and successful completion.
`no_adapter_declares_image_input_without_a_replayed_contract` runs that proof
for the supported image capability at the install pin. A matching filename alone
cannot qualify image input. This proves transport compatibility with the fixture;
it does not certify image interpretation or a fresh run of the install pin.

## Record a protocol change

There is no general capture binary. The removed helper resolved engines through
the login shell and did not support ACP or MCP elicitation. Use a targeted
protocol driver for the scenario and record how you invoked it in the manifest.

1. Resolve the exact managed engine through `pin::managed_binary` or
   `pin::managed_binary_version`. Record its absolute executable, version output,
   argv, protocol, model or scripted provider, and capture date. Do not substitute
   an engine found on the login-shell PATH.
2. Use an isolated home and throwaway workspace. A scripted local provider can
   prove protocol behavior without paid inference; label that scope explicitly.
3. Record both directions where the protocol needs request/response matching.
   Preserve approval replies, cancellation, resumption, and terminal outcomes for
   the scenario you are testing.
4. Redact the stream, register it in the manifest, and add a replay with assertions
   that fail when the behavior you depend on changes.

The existing integration shapes are:

- Claude: `--input-format stream-json --output-format stream-json --verbose
  --include-partial-messages`. The 2.1.233 prompt-tool approval captures include
  the MCP `tools/call` request and allow/deny responses. That hidden
  `--permission-prompt-tool` flag is not listed in the captured `--help`.
- Codex: `app-server --stdio`, framed as `{"dir":"in"|"out","msg":{…}}`.
  The 0.147.0 baseline includes command approvals; 0.153.0 adds
  `mcpServer/elicitation/request` for MCP tools.
- opencode: `serve --hostname 127.0.0.1 --port N`, framed HTTP and SSE records.
  The baseline includes `/session`, `/prompt_async`, `/event`, and
  `/permission/{id}/reply`.
- Grok: 1.0.4 print-mode `--output-format streaming-json`; 1.0.13 also records
  `agent --no-leader stdio` ACP. The image probe proves a `read_file` tool image
  reaches the next model request, not direct user-image attachments.

Read each version's manifest before changing its parser. Check the managed
engine's help and behavior before recording a different version.

## Redaction and replay

Before committing a stream:

1. Replace home and workspace paths with `/workspace` where only a cwd matters.
2. Remove API keys, bearer tokens, cookies, and thinking signatures.
3. Replace host-local sockets and temporary paths with `/tmp/redacted.sock`.
4. Preserve event types, tool names, argument shapes, and identifiers needed to
   match requests, approvals, turns, or resumed sessions.
5. Record every redaction in the manifest. Keep hook and status events so parsers
   must tolerate them.

To generate an expected sequence, run the affected replay with:

```text
UPDATE_HARNESS_FIXTURES=1 cargo test -p tidebreak-harness --locked fixture_replay_image_input
```

Replace the test filter for another scenario. Inspect the expected diff, then
run the same test without `UPDATE_HARNESS_FIXTURES`. Add protocol branches only
when a recorded stream or an explicitly labelled regression fixture supports
the shape.
