<p align="center">
  <br>
  <img src="assets/tidebreak-mark.svg" alt="Tidebreak mark" width="300">
</p>

<h1 align="center">Tidebreak</h1>

<p align="center">
  <strong>A local-first agentic coworker you can configure without limit. Choose your files and model. Walk away with a spreadsheet, deck, or working app.</strong>
</p>

<p align="center">
  <a href="https://www.tidebreak.io">Website</a> ·
  <a href="#downloads">Download</a> ·
  <a href="https://www.tidebreak.io/docs/quickstart/">Quickstart</a> ·
  <a href="https://www.tidebreak.io/docs/">Documentation</a> ·
  <a href="CONTRIBUTING.md">Contributing</a>
</p>

<p align="center">
  <a href="https://github.com/brightwave-inc/tidebreak/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/brightwave-inc/tidebreak/ci.yml?branch=main&amp;style=flat-square&amp;logo=githubactions&amp;logoColor=white&amp;label=CI" alt="CI status"></a>
  <a href="https://github.com/brightwave-inc/tidebreak/releases/latest"><img src="https://img.shields.io/github/v/release/brightwave-inc/tidebreak?style=flat-square&amp;logo=github&amp;label=release" alt="Latest release"></a>
  <a href="https://github.com/brightwave-inc/tidebreak/releases"><img src="https://img.shields.io/github/downloads/brightwave-inc/tidebreak/total?style=flat-square&amp;logo=github&amp;label=downloads" alt="Total downloads"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/brightwave-inc/tidebreak?style=flat-square&amp;label=license" alt="Apache-2.0 license"></a>
  <a href="https://www.tidebreak.io/docs/roadmap/"><img src="https://img.shields.io/badge/status-pre--1.0-F59E0B?style=flat-square" alt="Project status: pre-1.0"></a>
</p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/tidebreak-hero-dark.png">
    <img src="assets/tidebreak-hero-light.png" alt="Tidebreak coordinating four background agents and previewing the launch-readiness brief they produced">
  </picture>
</p>

---

Tidebreak is a local-first desktop agent for work that ends in a file, not just
a chat response. It can read attached documents, work in explicitly connected
folders, run code in a sandbox, search the web, delegate parallel jobs, and
publish anything it creates as a versioned output. Code mode turns the same
supervision onto software work: it drives coding agents such as Claude Code in
isolated git worktrees, with native approvals and per-turn diffs.

You choose the model route: Anthropic, OpenAI, Google, xAI, OpenRouter, a local
Ollama daemon, or another OpenAI-compatible endpoint. Conversations can switch
providers without starting over. Credentials stay in the operating system's
credential store, while chats and outputs remain in the local Tidebreak
profile.

The [website](https://www.tidebreak.io) is the product overview. The
[user documentation](https://www.tidebreak.io/docs/) covers installation,
configuration, permissions, connected folders, code execution, outputs, and
the headless CLI.

> [!WARNING]
> Tidebreak is pre-1.0 and changing quickly. Interfaces and local data formats
> may change between releases, and profiles may be rebuilt rather than
> migrated. Windows and Linux packages ship for x86_64 and ARM64; some native
> capabilities remain macOS-only and are reported as unavailable in the app.

## Downloads

| Platform | Latest packages | Notes |
| --- | --- | --- |
| **macOS** | [Universal `.dmg`](https://github.com/brightwave-inc/tidebreak/releases/latest/download/Tidebreak-macos-universal.dmg) | Apple Silicon and Intel; signed and notarized |
| **Windows** | [x86_64 installer](https://github.com/brightwave-inc/tidebreak/releases/latest/download/Tidebreak-windows-x86_64-setup.exe) · [ARM64 installer](https://github.com/brightwave-inc/tidebreak/releases/latest/download/Tidebreak-windows-aarch64-setup.exe) | Windows may show a SmartScreen warning while installers are not Authenticode-signed |
| **Linux** | x86_64 [AppImage](https://github.com/brightwave-inc/tidebreak/releases/latest/download/Tidebreak-linux-x86_64.AppImage) / [`.deb`](https://github.com/brightwave-inc/tidebreak/releases/latest/download/Tidebreak-linux-x86_64.deb) · ARM64 [AppImage](https://github.com/brightwave-inc/tidebreak/releases/latest/download/Tidebreak-linux-aarch64.AppImage) / [`.deb`](https://github.com/brightwave-inc/tidebreak/releases/latest/download/Tidebreak-linux-aarch64.deb) | Built on Ubuntu 22.04; use a compatible glibc-based distribution |

Every package has a `.sha256` sidecar on the release. See the
[installation guide](https://www.tidebreak.io/docs/installation/) for checksum
commands, Linux runtime requirements, update behavior, and platform-specific
limitations.

## Quick start

1. Install the latest desktop build above and open Tidebreak.
2. In **Settings → Providers**, sign in with a ChatGPT Plus or Pro subscription,
   add a provider API key, or configure a local Ollama/OpenAI-compatible model.
3. Start a chat, attach a file with `@` or connect a folder, choose a permission
   mode, and ask for a concrete output: a cleaned spreadsheet, a deck, a report,
   a chart, or a working app.
4. Open files from the **Outputs** panel and export the version you want.

The [full quickstart](https://www.tidebreak.io/docs/quickstart/) walks through a
first file-backed task and explains the approval prompts you will see.

## Features

| Area | What it does |
| --- | --- |
| **Chat** | Voice input, local or cloud. Queue a follow-up or steer the live turn. `@` attaches files and folders; `/` attaches a skill or saved prompt. Turns, approvals, and questions survive a restart. An inbox collects anything waiting on you. |
| **Documents** | Attach any file. Office, PDF, images, and text open in native viewers; other formats fall back to extracted text when they can be read. Source pills jump to the page, line, sheet, or cell. No vector index: the agent reads bounded ranges directly. |
| **Outputs** | Files written to `output/` are versioned by filename. Restore is append-only. Plotly charts stay interactive. The agent can write back into a connected folder; replacing an existing file always asks. |
| **Models** | ChatGPT Plus or Pro, an API key (Anthropic, OpenAI, Gemini, xAI, Fireworks, Together, OpenRouter), or a local OpenAI-compatible endpoint including Ollama. Switch mid-chat. Keys stay in the OS credential store. |
| **Execution** | Native macOS sandbox, local Docker, E2B, or Daytona. Per-chat network policy. Background agents one level deep. Built-in or configured web search (Exa, Tavily, Brave, SearXNG). Computer use: screen capture and consent-gated control of native apps. |
| **Permissions** | Plan, Ask, Auto, or Allow all, per chat. Folder access is per capability from a native picker. Standing grants are revocable. Overwrite of a connected file always asks. |
| **Extensions** | Built-in skills for Word, PDF, PowerPoint, spreadsheets, and charts; add your own. MCP servers over stdio or HTTP. REST APIs from an OpenAPI document, for local apps. Ask once for a mini-app and keep it in the sidebar. |
| **Code mode** | A second surface that drives coding agents (Claude Code, Codex CLI, opencode, Grok CLI) in isolated git worktrees: native approvals, per-turn diffs, and a reviewable change flow. |

## Data, privacy, and security

Tidebreak does not require a Tidebreak account or route work through a hosted
Tidebreak service. Chats, attached documents, and outputs live in the local app
profile, and provider credentials live in the operating system's credential
store.

Local-first does not automatically mean offline. The selected model provider may
receive relevant prompts and document content; search providers receive search
queries; configured APIs and MCP servers receive the calls made to them; and
explicit web fetches reach the requested sites. Use a local model, avoid external
integrations, and select an offline execution policy when a workflow must remain
on the machine.

Permission modes govern what the agent may do without asking, while the selected
execution backend and per-chat network policy govern where code runs and what it
can reach. Start with the [permission guide](https://www.tidebreak.io/docs/permission-modes/)
and [code-execution guide](https://www.tidebreak.io/docs/code-execution/). Report
vulnerabilities privately through [`SECURITY.md`](SECURITY.md), not a public
issue.

## Run from source

Install [rustup](https://rustup.rs/) (which picks up the repository's pinned Rust
toolchain), Node.js 22, [pnpm](https://pnpm.io), the
[Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/), the Tauri
CLI, and CMake (used by the local voice engine):

```sh
cargo install tauri-cli --version "^2"
```

Then, from the repository root on macOS or Linux:

```sh
scripts/dev.sh
```

The script checks the prerequisites, installs the desktop UI dependencies, and
opens the app. On Windows, or without Bash, run the equivalent commands:

```sh
cd crates/tidebreak-desktop
pnpm --dir ui install
cargo tauri dev
```

See
[`CONTRIBUTING.md`](CONTRIBUTING.md#development) for formatting, tests, and
platform notes, or
[`crates/tidebreak-desktop/README.md`](crates/tidebreak-desktop/README.md) for
the native and browser-based UI workflows.

The headless server uses the same core runtime:

```sh
ANTHROPIC_API_KEY=sk-... cargo run -p tidebreak-cli -- serve
```

See the [headless documentation](https://www.tidebreak.io/docs/headless/) for
one-shot mode, HTTP API, MCP server, configuration, and self-hosting.

## Repository guide

| Path | Purpose |
| --- | --- |
| [`crates/`](crates) | Rust workspace: agent runtime, providers, server, CLI, sandboxing, MCP, and desktop host |
| [`crates/tidebreak-desktop/ui/`](crates/tidebreak-desktop/ui) | React frontend embedded by the Tauri desktop app |
| [`docs-site/`](docs-site) | Source for the public user documentation |
| [`docs/`](docs) | Maintainer architecture, contracts, operations, plans, and decision records |
| [`skills/`](skills) and [`plugins/`](plugins) | Bundled artifact skills and plugin manifests |
| [`deploy/self-host/`](deploy/self-host) | Self-host deployment assets |

Start with [`docs/how-tidebreak-works.md`](docs/how-tidebreak-works.md) for an
end-to-end technical tour or [`docs/crates.md`](docs/crates.md) for the
workspace map. [`docs/README.md`](docs/README.md) indexes the rest of the
maintainer documentation.

## Contributing

Tidebreak is built in the open. Bug reports, focused fixes, and design
discussion are welcome. Read [`CONTRIBUTING.md`](CONTRIBUTING.md) before
opening a substantial change. Decisions that future work must preserve live in
[`docs/decisions/`](docs/decisions), and all participation is covered by the
[`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).

## Support

- Read the [troubleshooting guide](https://www.tidebreak.io/docs/troubleshooting/)
  for installation, provider, execution, and local-state problems.
- [Open a bug report](https://github.com/brightwave-inc/tidebreak/issues/new?template=bug-report.yml)
  with a reproducible case and [diagnostics](docs/diagnostics.md) scrubbed of
  sensitive data.
- [Request a feature](https://github.com/brightwave-inc/tidebreak/issues/new?template=feature-request.yml)
  by describing the workflow and desired outcome.
- Follow [`SECURITY.md`](SECURITY.md) to report a vulnerability privately.

## License

Apache-2.0. See [`LICENSE`](LICENSE), [`NOTICE`](NOTICE), and the
[contributor license agreement](CONTRIBUTING.md#contributor-license-agreement).
