# Harness fixtures

Adapter parsers may only be written or modified against captured streams from
a real engine invocation. Each capture lives under
`<harness>/<version>/` with:

- `<scenario>.ndjson` — the raw protocol stream, one JSON object per line
- `<scenario>.expected.json` — the normalized `HarnessEvent` sequence the
  parser must produce
- `manifest.toml` — exact argv, observed version, date, and redaction notes

CI replays fixtures. It cannot capture them.

## Capture

The engine must be installed and signed in on the capturing machine. From the
workspace root:

```text
cargo run -p tidebreak-harness --features capture --bin tidebreak-harness-capture -- \
  --harness claude-code \
  --scenario plain-text \
  --prompt "reply with exactly: hello from fixture"
```

The binary creates a throwaway git repo, runs the engine in print mode, tees
stdout to `fixtures/<harness>/<version>/<scenario>.ndjson`, and writes
`manifest.toml`.

Use the cheapest model the engine accepts, tiny prompts, and the smallest
tool allowlist that still produces the scenario (for example a single `Read`
for a tool-use turn). Check `claude --help` for current flags; the capture
bin defaults to `-p --output-format stream-json --verbose --include-partial-messages`.

Re-capture the whole version directory when the engine version moves.

## Redaction

Before committing a capture:

1. Strip absolute home paths (`/Users/…`, `/home/…`). Replace the worktree
   with `/workspace` when the path is only a cwd.
2. Strip anything token-like: API keys, bearer tokens, `sk-…` strings,
   cookie headers, thinking signatures.
3. Replace host-local sockets and paths (`/var/folders/…`,
   `$TMPDIR/…/*.sock`) with `/tmp/redacted.sock`.
4. Keep structural fidelity: event `type`s, tool names, session ids that the
   resume fixture needs, and argument *shapes*.
5. Record every redaction in `manifest.toml` under `redaction_notes`.

Real streams include user-hook `system` events (`hook_started`,
`hook_response`). Leave them in. The parser must tolerate them.

After redaction, regenerate expected sequences:

```text
UPDATE_HARNESS_FIXTURES=1 cargo test -p tidebreak-harness --locked
```

Do not invent parser branches for shapes that are not in a fixture.
