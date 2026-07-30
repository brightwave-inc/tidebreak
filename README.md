<p align="center">
  <img src="assets/openwave-logo.png" alt="OpenWave" width="380">
</p>

<h1 align="center">OpenWave</h1>

<p align="center">
  <strong>An open, local-first cowork runtime.</strong><br>
  A Rust-native agent that works over your files and tools — on your machine, with your own model keys.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License: Apache-2.0">
  <img src="https://img.shields.io/badge/status-pre--alpha-orange.svg" alt="Status: pre-alpha">
  <img src="https://img.shields.io/badge/built%20with-Rust-dea584.svg?logo=rust&logoColor=white" alt="Built with Rust">
  <a href="https://github.com/brightwave-inc/openwave/releases/latest"><img src="https://img.shields.io/github/v/release/brightwave-inc/openwave?label=release&color=informational" alt="Latest release"></a>
</p>

<p align="center">
  <a href="https://github.com/brightwave-inc/openwave/releases/latest/download/OpenWave-macos-apple-silicon.dmg"><img src="https://img.shields.io/badge/Download%20for%20macOS-Apple%20Silicon-000000.svg?logo=apple&logoColor=white" alt="Download for macOS, Apple Silicon"></a>
</p>

---

> [!WARNING]
> **Early and in active development.** OpenWave is being built in the open, one
> slice at a time. Interfaces, schema, and crate layout change frequently, and
> it isn't ready for real-world use yet. Star the repo to follow along — and
> expect rough edges.

## Download

The desktop app ships for macOS, signed and notarized. The two links above
always resolve to the newest release; the
[releases page](https://github.com/brightwave-inc/openwave/releases) has the
notes and earlier versions. An installed build keeps itself current — it checks
for a newer signed release in the background and asks before restarting.

Each download has a `.sha256` sidecar on the same release if you want to verify
it:

```sh
shasum -a 256 -c OpenWave-macos-apple-silicon.dmg.sha256
```

There are no Windows or Linux builds yet; on those platforms, build from source
as described under [Building](#building).

## Where this comes from

OpenWave is the open core of the [Brightwave](https://brightwave.io) platform.

We've spent years building Brightwave — an agentic system for demanding,
real-world knowledge work: a runtime that plans and executes multi-step tasks,
runs **fleets of agents in parallel** inside sandboxes, connects to your
sources, and grounds its answers in citations. Skills and prompts are features
on top of that runtime, not the point of it. **We're taking the core pieces of
that platform — the agent loop, the tool model, sandboxing, connectors, and
source tools — and opening them up** under Apache-2.0, rebuilt lean in Rust as a
local-first application you run yourself.

The idea is simple: the engine that does the work shouldn't be a cloud service
that holds your data and meters your tokens. It should be something you own —
local by default, model-agnostic, and open.

The open version is **configurable end to end**. Bring your own API keys to
connect the LLMs you prefer behind a common interface, with sensible,
local-friendly defaults out of the box, so it works on your machine from the
first run.

## Why

Most agentic tools are cloud services that hold your data and meter your usage.
OpenWave is the opposite: a slim desktop app (plus a headless mode) that runs the
agent loop **on your machine**, keeps your data local, and lets you bring your
own model — hosted or fully offline. Its MCP server foundation can expose the
same tool registry to external agents; `openwave mcp <workspace>` serves the
built-in read-only file tools today. The inverse client foundation can initialize
external stdio MCP servers configured at boot and mount their tools into that
registry; configuration UI remains in development.

## Principles

- **Local-first & private.** Your files, keys, and history stay on your machine.
- **Bring your own keys.** Configure Anthropic, OpenAI, or an OpenAI-compatible
  endpoint (vLLM, LM Studio, Ollama, OpenRouter). We never meter tokens.
- **Slim by default.** Small install; no bundled model weights or language
  runtimes — fetched on first use, cached locally.
- **Composable tool surface.** The MCP server foundation exposes OpenWave's
  tools, while its client foundation mounts namespaced tools from external stdio
  MCP servers behind the same approval-aware registry.
- **Open core.** The runtime is Apache-2.0 and complete on its own.

## Status

Pre-alpha, built in the open. The current product is conversation-first: each
chat is its own workspace, with exact conversation-scoped sources rather than a
shared fallback corpus. Sources can be added from the composer and read directly
after synchronous text decoding, with model-authored citations. Foreground
agents can also create bounded text deliverables that remain private to the
conversation until the user previews and explicitly exports them from the
native Outputs view. The stack includes local file tools, multi-provider model
routing, a turn engine with live journaled WebSocket events, a workspace-style
desktop conversation shell, a bounded foreground/sandbox agent-run foundation,
and durable synchronous source ingestion and reading — all behind
`openwave serve`.
Project records and APIs remain dormant for compatibility and future design
work, but Projects are not surfaced in the desktop and are not required to
start working.

See [`docs/deliverables.md`](docs/deliverables.md) for the generated-output and
native export boundary.

Connectors, richer document parsers, indexed-search MCP wiring, and MCP
configuration UI remain in development. Expect rapid change and rough edges —
and see [CONTRIBUTING](CONTRIBUTING.md) if you'd like to help.

## Building

```sh
cargo build --workspace
cargo test --workspace
```

Requires the Rust toolchain declared in [`rust-toolchain.toml`](rust-toolchain.toml).

### Desktop app (Tauri)

The desktop shell lives in [`crates/openwave-desktop`](crates/openwave-desktop).
It boots the same local API as `openwave serve` inside the process and hosts a
React UI in a webview. See that crate's README for prerequisites and the two
local-test paths (`cargo tauri dev`, or browser UI against `openwave serve`).

Headless API without the UI:

```sh
ANTHROPIC_API_KEY=sk-... cargo run -p openwave-cli -- serve
# then: curl -s http://127.0.0.1:PORT/healthz
#       curl -H "Authorization: Bearer TOKEN" http://127.0.0.1:PORT/chats

# MCP stdio server confined to an explicit workspace (read_file + list_dir):
cargo run -p openwave-cli -- mcp /absolute/path/to/workspace
```

### External MCP servers

Both `openwave serve` and the desktop app can mount external stdio MCP servers
at startup. Set `OPENWAVE_MCP_CONFIG` to a JSON file:

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
OPENWAVE_MCP_CONFIG=/absolute/path/to/mcp.json \
  cargo run -p openwave-cli -- serve
```

Commands are executed directly, without a shell. Each server receives only its
configured literal `env` and the parent variables explicitly named by
`env_from`, so use an absolute command path. A missing `env_from` variable fails
startup; set `"inherit_env": true` only when the server must inherit OpenWave's
entire process environment. Prefer `env_from` for credentials so they need not
be stored in JSON. Treat the file as sensitive and restrict its filesystem
permissions if it does contain credentials. Startup fails if a configured
server cannot initialize. Its discovered tools are named
`mcp__{server}__{tool}` and always cross the sensitive-tool approval boundary.

## Layout

This is a single Cargo workspace. Libraries never depend on clients — the
dependency graph only flows downward toward `openwave-core`. For a fuller
walkthrough of each crate, see [`docs/crates.md`](docs/crates.md). For an
end-to-end, less technical tour of the runtime and its state machines, see
[`docs/how-openwave-works.md`](docs/how-openwave-works.md). The planned shared
foreground/background execution model is described in
[`docs/agent-runs.md`](docs/agent-runs.md).

| Crate | What it is |
| --- | --- |
| [`openwave-core`](crates/openwave-core) | agent loop, tools, event stream, storage traits |
| [`openwave-router`](crates/openwave-router) | Anthropic and OpenAI-compatible providers + model routing |
| [`openwave-code-execution`](crates/openwave-code-execution) | provider-neutral command execution + native local sandbox |
| [`openwave-server`](crates/openwave-server) | authenticated local HTTP/WebSocket API + durable workers |
| [`openwave-connectors`](crates/openwave-connectors) | OAuth + source connectors |
| [`openwave-mcp`](crates/openwave-mcp) | lifecycle-gated MCP server plus external stdio client tool mounting |
| [`openwave-desktop`](crates/openwave-desktop) | desktop app (Tauri) |
| [`openwave-cli`](crates/openwave-cli) | headless `openwave serve` + `openwave mcp` commands |

## License

Apache-2.0 — see [LICENSE](LICENSE) and [NOTICE](NOTICE). Contributions are
accepted under the terms in
[CONTRIBUTING](CONTRIBUTING.md#contributor-license-agreement).
