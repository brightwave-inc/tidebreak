<p align="center">
  <br>
  <img src="assets/tidebreak-mark.svg" alt="Tidebreak mark" width="300">
</p>

<h1 align="center">Tidebreak</h1>

<p align="center">
  <strong>A private AI coworker that runs on your machine.</strong><br>
  Point it at your files and tools, and it produces finished, versioned work —
  and reusable local apps — without your data leaving your computer.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License: Apache-2.0">
  <img src="https://img.shields.io/badge/status-pre--1.0-orange.svg" alt="Status: pre-1.0">
  <img src="https://img.shields.io/badge/built%20with-Rust-dea584.svg?logo=rust&logoColor=white" alt="Built with Rust">
  <a href="https://github.com/brightwave-inc/tidebreak/releases/latest"><img src="https://img.shields.io/github/v/release/brightwave-inc/tidebreak?label=release&color=informational" alt="Latest release"></a>
</p>

<p align="center">
  <a href="https://github.com/brightwave-inc/tidebreak/releases/latest/download/Tidebreak-macos-apple-silicon.dmg"><img src="https://img.shields.io/badge/Download%20for%20macOS-000000.svg?logo=apple&logoColor=white" alt="Download the latest macOS release"></a>
</p>

---

> [!WARNING]
> **Pre-1.0 and moving fast.** Tidebreak is built in the open. Interfaces,
> schema, and local data layout still change between releases, and the local
> profile is rebuilt rather than migrated when it does. Expect rough edges.

## What it does

A chat is a workspace. You attach files, connect folders, choose a model, and
ask for the thing you actually want — a model, a deck, a cleaned dataset, a
triage view. The agent runs code in a sandbox to build it, and what comes back
is a file you can open, not a wall of text.

**Finished work, kept as versions.** Files the agent writes to its workspace
`output/` directory are published into the chat's **Outputs** catalog. Writing
the same filename again appends a new version rather than overwriting one, so
every output carries a history of who produced each version — a foreground
turn, a background agent, or you — and when. Restore is append-only: restoring
v2 while you're on v5 produces v6, so nothing is rewound or lost, and a restore
can be undone by another restore. Delete is soft, with an inline undo. Outputs
stay private app data until you pick a destination through a native **Save
As…** dialog. Spreadsheets, CSV, Word documents, PDFs, and images preview
in-app. See [`docs/deliverables.md`](docs/deliverables.md).

**Work that runs in parallel, and survives a restart.** The foreground agent
can hand independent jobs to background agents — up to four unsettled from one
turn — each with its own isolated workspace, then join them at one explicit
point and get results back in the order it asked for. The jobs and their place
in the conversation are durable database state, so closing the app or
restarting the server does not lose them. A background panel shows what is
queued, running, or finished, and lets you stop a run. Background agents cannot
spawn further agents. See [`docs/agent-runs.md`](docs/agent-runs.md).

**Real folders, on explicit terms.** A folder elsewhere on your machine becomes
available only after you choose it in a native picker, and authority is
attached to the exact conversation. A new chat inherits nothing and there is no
shared fallback workspace. The model sees an opaque root ID and root-relative
paths, never an absolute host path, and connecting one folder grants nothing
about its siblings or your home directory. The agent can ask for another
folder and suggest where to open the picker, but only what you select is
granted. See [`docs/host-access.md`](docs/host-access.md).

**Local apps you can reopen.** Ask once for something like a triage view over a
folder of exports, and you get a durable mini-app in the sidebar instead of a
conversation you re-run. An app is model-authored HTML rendered in a sandboxed
frame with no network of its own, plus a small manifest pinning exactly which
connected-app operations and folders it may call. You consent to the manifest,
not the HTML; the host enforces the pin on every call, and reconfiguring a
bound server invalidates the grant so consent cannot outlive what it named.
Apps live in the profile, so they outlive the chat that created them. See
[`docs/local-apps.md`](docs/local-apps.md).

Alongside those: web search and page extraction through Exa, Tavily, Brave, or
a self-hosted SearXNG ([`docs/web-search.md`](docs/web-search.md)); MCP servers
and REST APIs as connected apps ([`docs/connected-apps.md`](docs/connected-apps.md));
and bundled skills for charts, spreadsheets, presentations, Word documents, and
PDFs under [`skills/`](skills) and [`plugins/`](plugins).

## Download

Tidebreak ships as one signed and notarized **universal macOS app** for Apple
Silicon and Intel ([`docs/releases.md`](docs/releases.md)). The button above
always resolves to the newest release; the
[releases page](https://github.com/brightwave-inc/tidebreak/releases) has the
notes and earlier versions.

Every release is also published to the hosted download root, which is the
authoritative source for artifact bytes and digests:

```text
https://downloads.brightwave.io/tidebreak/manifest.json
```

That manifest names the current version's DMG along with its size and SHA-256,
so you can verify what you downloaded:

```sh
shasum -a 256 -c Tidebreak_<version>_aarch64.dmg.sha256
```

An installed macOS build keeps itself current: it checks the release feed
shortly after launch and every five minutes, downloads a newer signed version
in the background, and waits for you to choose **Restart to update**. It never
interrupts active work. Development builds do not contact the production feed.

**Windows and Linux have no packaged build right now.** Windows installers
shipped through v0.34.0, but the Windows CI and release-build lanes are
currently parked for build-cost reasons — the packaging code is still in the
tree and nothing about Windows support has been withdrawn, but releases are
macOS-only until those lanes are turned back on, and a Windows user on v0.34.0
has no upgrade path in the meantime. Linux has never had a packaged build. On
both, run from source as described under [Building](#building), and read the
platform differences below first.
[`docs/deferred.md`](docs/deferred.md) records the full reasoning.

## Model and provider portability

Provider credentials go in the OS keychain, not in the database and not in a
config file you might commit. Tidebreak talks to Anthropic, OpenAI and
OpenAI-compatible endpoints, Google Gemini, xAI, OpenRouter, and a local
Ollama daemon.

The conversation journal is provider-neutral, so a chat can move from Claude to
Gemini to GPT and back without starting over — useful when a provider is rate
limiting you mid-task, and useful for running the cheap model through the
grind and the strong model through the hard step without forking context.
Switching routes is a deliberate flattening: provider-native artifacts such as
thinking-block signatures are origin-gated and degrade to plain content rather
than being translated pairwise. Providers are explicitly tiered by capability
rather than assumed equivalent. See
[`docs/model-providers.md`](docs/model-providers.md).

## How it stays safe

**Four permission modes, per chat.** **Plan** is read-only — mutating calls are
refused outright rather than parked for approval, so a planning turn cannot
change anything no matter what you approve. **Ask** (the default) parks every
uncovered mutating call on an approval card. **Auto** is a standing yes to
workspace edits while anything leaving the workspace still asks. **Allow all**
is an explicit per-chat opt-in to full autonomy. One exception overrides every
mode: replacing a file that already exists in a connected folder always asks,
because no mode should advertise consent for destroying bytes the agent did not
write. Approval cards are durable and survive a restart, and "always allow" can
be scoped from one exact command up to a whole tool for that chat.

**Code runs in a sandbox.** `exec` goes through a provider-neutral contract:
the native macOS sandbox, a local Docker container, or a managed E2B or
Daytona workspace. Provider-native responses, credentials, and unbounded logs
never cross the contract, and absolute host paths are a host-only input the
model never sees.
[`docs/code-execution.md`](docs/code-execution.md) documents each backend,
including how strictly each one can actually enforce a network policy — the
statuses differ, and the document states them plainly rather than implying
parity.

**Versioning is the undo.** Because output history is insert-only and restore
appends, the agent producing a wrong file is a recoverable event rather than a
lost one. That is what makes an ungated workspace write acceptable in the first
place.

## Current limitations

Worth knowing before you install:

- **Packaged builds are macOS only, for Apple Silicon and Intel.** See
  [Download](#download).
- **The native execution sandbox is macOS-only.** On Linux or Windows there is
  no native confinement primitive; the choices are the opt-in local Docker
  backend or a managed cloud provider that receives the chat's staged files.
  The Docker backend enforces only a chat set to no network, which it runs with
  `--network none`; allowlist and package-manager policies are not enforced
  there.
- **Connected-folder access inside code execution is macOS-only.** Managed
  providers cannot reach host folders at all.
- **No semantic search or indexed retrieval.** There is no corpus, no
  embeddings, and no vector index. Documents are read directly, and files plus
  code execution are the whole interface to your data. This is a deliberate
  narrowing, not a gap waiting to be filled.
- **No scheduled or recurring work.** Background agents make work durable, but
  nothing runs on a timer yet.
- **No browser automation.**
- **One local user.** The loopback bearer token is a capability check on the
  local process, not a per-user identity. Tidebreak is a single-user desktop
  product today; do not put more than one person behind one server.
- **Local data is not migrated across pre-1.0 releases.** The desktop schema
  guard rebuilds the profile when the shape changes.

[`docs/deferred.md`](docs/deferred.md) is the canonical account of what
Tidebreak deliberately does not do yet and why.

## Where this comes from

Since 2024 we've built [Brightwave](https://brightwave.io), a research platform
for financial diligence used by finance professionals for work they put their
name on.

Running it in production taught us some things. The agent loop is the easy
part; the hard part is what happens when a tool call fails halfway through a
twenty-step task. Sandboxing has to be there from the start. A user who can't
switch models is stuck with their vendor's outages and their vendor's pricing.
And the thing people actually want at the end is a file, not a transcript.

Tidebreak is that engine rebuilt in Rust — the agent loop, the tool model,
sandboxed execution, connected apps, and permissions — Apache-2.0, local-first,
and complete on its own.

## Building

```sh
cargo build --workspace
cargo test --workspace
```

Requires the Rust toolchain declared in
[`rust-toolchain.toml`](rust-toolchain.toml).

### Desktop app (Tauri)

The desktop shell lives in [`crates/tidebreak-desktop`](crates/tidebreak-desktop).
It boots the same local API as `tidebreak serve` inside the process and hosts a
React UI in a webview. See that crate's README for prerequisites and the two
local-test paths (`cargo tauri dev`, or the browser UI against
`tidebreak serve`).

### Headless

```sh
ANTHROPIC_API_KEY=sk-... cargo run -p tidebreak-cli -- serve
# then: curl -s http://127.0.0.1:PORT/healthz
#       curl -H "Authorization: Bearer TOKEN" http://127.0.0.1:PORT/chats
```

The CLI also has an interactive terminal chat (`tidebreak tui`), a
non-interactive single turn (`tidebreak -p "<prompt>"`, with
`--output-format json` for the turn's NDJSON event stream), and an MCP stdio
server confined to one explicit workspace:

```sh
cargo run -p tidebreak-cli -- mcp /absolute/path/to/workspace
```

Note that host-brokered connected folders and the native execution sandbox are
desktop and macOS features; the headless server does not provide them.

### External MCP servers

The desktop configures MCP servers under Settings; see
[`docs/mcp-servers.md`](docs/mcp-servers.md). For headless use, both
`tidebreak serve` and the desktop app can also mount external stdio MCP servers
at startup from a JSON file named by `TIDEBREAK_MCP_CONFIG`:

```json
{
  "servers": [
    {
      "name": "private_docs",
      "command": "/absolute/path/to/docs-mcp",
      "args": ["--stdio"],
      "env": { "LOG_LEVEL": "info" },
      "env_from": ["DOCS_TOKEN"],
      "cwd": "/srv/docs",
      "request_timeout_ms": 60000
    }
  ]
}
```

```sh
TIDEBREAK_MCP_CONFIG=/absolute/path/to/mcp.json \
  cargo run -p tidebreak-cli -- serve
```

Commands are executed directly, without a shell. Each server receives only its
configured literal `env` and the parent variables explicitly named by
`env_from`, so use an absolute command path. A missing `env_from` variable fails
startup; set `"inherit_env": true` only when the server must inherit Tidebreak's
entire process environment. Prefer `env_from` for credentials so they need not
be stored in JSON. Treat the file as sensitive and restrict its filesystem
permissions if it does contain credentials. Startup fails if a configured
server cannot initialize. Its discovered tools are named
`mcp__{server}__{tool}` and always cross the sensitive-tool approval boundary.

## Layout

This is a single Cargo workspace. Libraries never depend on clients — the
dependency graph only flows downward toward `tidebreak-core`. For a fuller
walkthrough of each crate, see [`docs/crates.md`](docs/crates.md); for an
end-to-end, less technical tour of the runtime and its state machines, see
[`docs/how-tidebreak-works.md`](docs/how-tidebreak-works.md).

| Crate | What it is |
| --- | --- |
| [`tidebreak-core`](crates/tidebreak-core) | agent loop, tools, event stream, storage traits |
| [`tidebreak-router`](crates/tidebreak-router) | Anthropic, OpenAI, Google, xAI, and OpenAI-compatible providers + model routing |
| [`tidebreak-host-broker`](crates/tidebreak-host-broker) | consented access to folders on the host |
| [`tidebreak-code-execution`](crates/tidebreak-code-execution) | provider-neutral command execution + native, Docker, and managed backends |
| [`tidebreak-egress`](crates/tidebreak-egress) | egress policy decisions for outbound network access |
| [`tidebreak-sandbox-protocol`](crates/tidebreak-sandbox-protocol) | the sandbox-agent wire protocol |
| [`tidebreak-server`](crates/tidebreak-server) | authenticated local HTTP/WebSocket API + durable workers |
| [`tidebreak-mcp`](crates/tidebreak-mcp) | MCP server face plus external stdio client tool mounting |
| [`tidebreak-desktop`](crates/tidebreak-desktop) | desktop app (Tauri) |
| [`tidebreak-cli`](crates/tidebreak-cli) | headless `tidebreak serve`, `tui`, print mode, and `mcp` |

## Contributing

See [CONTRIBUTING](CONTRIBUTING.md). Design decisions live in
[`docs/decisions/`](docs/decisions) as numbered records.

## License

Apache-2.0 — see [LICENSE](LICENSE) and [NOTICE](NOTICE). Contributions are
accepted under the terms in
[CONTRIBUTING](CONTRIBUTING.md#contributor-license-agreement).
