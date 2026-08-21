# Browser bridge E2E tests

Deterministic pure-Node shipped-path contract tests for the agent
browser channel (`/code/browser/list`, `/navigate`, `/snapshot`).

## What this covers

These tests exercise the real v1 capfile shape `{version:1, endpoint, token}`
and the real HTTP route shapes against a loopback-only mock bridge server.
They prove the provider-neutral contract that every engine adapter (Claude,
Codex, OpenCode, Grok) consumes — the same JSON shapes, status codes, error
kinds, and capability-file protocol the real CLI and MCP bridge speak.

Coverage includes:

- Exact tool registry (only `browser_list`, `browser_navigate`,
  `browser_snapshot`; `act`/`wait`/`screenshot` absent).
- Capfile v1 schema validation and token secrecy.
- Positive contract: list → navigate → snapshot with camelCase shape
  assertions.
- Absolute bridge command with `PATH` unavailable; token never appears in
  argv, stdout, stderr, or capfile path.
- Negative matrix: missing/unknown/revoked token (401), ended session
  (403), cross-workspace browser id (404), stopped control, hidden
  browser, stale document epoch (409), invalid/credentialed URLs (422),
  unknown fields in body (400), missing required fields (400),
  out-of-range max_nodes (422), redirect refusal, missing runtime (501).
- Provider-neutral `camelCase` response shapes.

## What this does NOT cover

These tests do **not** prove any of the following (CI-only Rust assertions):

- `BrowserTokenRegistry` transactionality, TOCTOU safety, atomic
  capfile writes, or Unix permissions.
- Real `browser-mcp` MCP protocol bytes or `tools/list` against a
  running `tidebreak` binary.
- Desktop `BrowserRuntime` adapter driving a native browser engine.
- Provider adapter argv/env emission (Claude `--mcp-config`, Codex
  `-c`, OpenCode config merge, Grok prompt append).
- Real CLI capfile validation or bounded response body decoding.

The mock server is intentionally separate from the native
`BrowserRegistry`. It implements the HTTP contract only.

## Running

Requires Node 22 (uses `node:test`, `node:http`, `node:crypto`,
`node:fs/promises`).

```bash
node --test crates/tidebreak-desktop/tests/browser-bridge-e2e/bridge-e2e.test.mjs
```

No `npm install` or external dependencies needed. The test imports the
existing sibling fixture at `../browser-fixture/server.mjs`.

## Page content is untrusted

Every response from the bridge is page-originated content marked
`contentTrust: "untrusted_page"`. No assertion in these tests treats
page content as instruction, and no test follows page-supplied links
or executes page-supplied JavaScript. The mock bridge reproduces this
contract explicitly.

## Files

- `bridge-server.mjs` — loopback-only mock `/code/browser` HTTP server
  with deterministic browser state.
- `scripted-harness.mjs` — capfile protocol, bounded HTTP client,
  tool registry assertions, camelCase contract assertions, and
  simulated absolute-command launch.
- `bridge-e2e.test.mjs` — the test suite.
- `README.md` — this file.
