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
</p>

---

> [!WARNING]
> **Early and in active development.** OpenWave is being built in the open, one
> slice at a time. Interfaces, schema, and crate layout change frequently, and
> it isn't ready for real-world use yet. Star the repo to follow along — and
> expect rough edges.

## Where this comes from

OpenWave is the open core of the [Brightwave](https://brightwave.io) platform.

We've spent years building Brightwave — an agentic system for demanding,
real-world knowledge work: a runtime that plans and executes multi-step tasks,
runs **fleets of agents in parallel** inside sandboxes, connects to your
sources, and grounds its answers in citations. Skills and prompts are features
on top of that runtime, not the point of it. **We're taking the core pieces of
that platform — the agent loop, the tool model, sandboxing, connectors, and
retrieval — and opening them up** under Apache-2.0, rebuilt lean in Rust as a
local-first application you run yourself.

The idea is simple: the engine that does the work shouldn't be a cloud service
that holds your data and meters your tokens. It should be something you own —
local by default, model-agnostic, and open.

The open version is **configurable end to end**. Bring your own API keys to
connect the LLMs, embedding services, and document parsers you prefer — each
behind a common interface — with sensible, local-friendly defaults out of the
box, so it works on your machine from the first run.

## Why

Most agentic tools are cloud services that hold your data and meter your usage.
OpenWave is the opposite: a slim desktop app (plus a headless mode) that runs the
agent loop **on your machine**, keeps your data local, and lets you bring your
own model — hosted or fully offline. It speaks [MCP](https://modelcontextprotocol.io),
so it works with the agents and tools you already use.

## Principles

- **Local-first & private.** Your files, keys, and history stay on your machine.
- **Bring your own keys.** Configure the models (Anthropic, OpenAI, Google, or
  anything OpenAI-compatible — vLLM, LM Studio, Ollama, OpenRouter), embedding
  services, and document parsers you want — with local-friendly defaults. We
  never meter tokens.
- **Slim by default.** Small install; no bundled model weights or language
  runtimes — fetched on first use, cached locally.
- **Works with any agent.** An MCP server _and_ a client — OpenWave both exposes
  tools and consumes them.
- **Open core.** The runtime is Apache-2.0 and complete on its own.

## Status

Pre-alpha, built in the open. The walking skeleton runs today: projects and
chats, local file tools, a turn engine, and live event streaming over WebSocket,
all behind `openwave serve`. Desktop UI, connectors, and retrieval come next.
Expect rapid change and rough edges — and see [CONTRIBUTING](CONTRIBUTING.md) if
you'd like to help.

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
```

## Layout

This is a single Cargo workspace. Libraries never depend on clients — the
dependency graph only flows downward toward `openwave-core`. For a fuller
walkthrough of each crate, see [`docs/crates.md`](docs/crates.md).

| Crate | What it is |
| --- | --- |
| [`openwave-core`](crates/openwave-core) | agent loop, tools, event stream, storage traits |
| [`openwave-connectors`](crates/openwave-connectors) | OAuth + source connectors |
| [`openwave-retrieval`](crates/openwave-retrieval) | embeddings, vector search, citations |
| [`openwave-mcp`](crates/openwave-mcp) | MCP server & client |
| [`openwave-desktop`](crates/openwave-desktop) | desktop app (Tauri) |
| [`openwave-cli`](crates/openwave-cli) | headless daemon + CLI |
| [`openwave-slack`](crates/openwave-slack) | Slack adapter |

_Model-provider adapters and routing/failover live in a planned `openwave-router`
crate — see [`docs/crates.md`](docs/crates.md)._

## License

Apache-2.0 — see [LICENSE](LICENSE) and [NOTICE](NOTICE). Contributions are
accepted under the terms in
[CONTRIBUTING](CONTRIBUTING.md#contributor-license-agreement).
