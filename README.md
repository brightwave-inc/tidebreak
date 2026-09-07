<p align="center">
  <br>
  <img src="assets/tidebreak-mark.svg" alt="Tidebreak mark" width="300">
</p>

<h1 align="center">Tidebreak</h1>

<p align="center">
  <strong>The open-source, local-first desktop for AI coding agents and finished work.</strong>
</p>

<p align="center">
  Run Claude Code, Codex CLI, opencode, or Grok CLI in isolated Git worktrees. Ship reviewed pull requests, repair CI, or turn documents and data into versioned spreadsheets, decks, reports, and apps.
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
    <source media="(prefers-color-scheme: dark)" srcset="assets/tidebreak-code-dark.webp">
    <img src="assets/tidebreak-code-light.webp" alt="Tidebreak in Code mode running a coding agent in an isolated Git worktree, with pull request checks, review comments, and merge controls beside the conversation">
  </picture>
</p>

## What is Tidebreak?

Tidebreak is an Apache-2.0, local-first AI coding tool and agent workspace.
**Code mode** takes a Git repository from task prompt to reviewed pull request.
**Work mode** turns documents and folders into versioned spreadsheets,
presentations, reports, charts, and apps.

Tidebreak does not replace the coding agents or models you already use. It runs
Claude Code, Codex CLI, opencode, and Grok CLI inside durable workspaces, then
adds worktree isolation, agent control, diffs, terminals, browser testing,
source control, continuous integration (CI), reviews, and delivery.

You choose the model, tools, files, repository, and execution boundary. Bring a
ChatGPT Plus or Pro subscription, an API key from Anthropic, OpenAI, Google,
xAI, Fireworks, Together, or OpenRouter, a local Ollama model, or another
OpenAI-compatible endpoint. Switch models without restarting the conversation.

The desktop runs on macOS, Windows, and Linux without a Tidebreak account.
Provider credentials stay in the operating system's credential store. Chats,
outputs, code sessions, and workspace metadata stay in your local Tidebreak
profile unless you connect an external service.

Brightwave built the original engine for three years of private-equity
diligence, then rewrote it in Rust and released it as Tidebreak.

> [!WARNING]
> Tidebreak is pre-1.0. Interfaces and local data formats may change between
> releases. Windows and Linux packages ship for x86_64 and ARM64, but some
> platform-specific capabilities remain macOS-only and appear as unavailable
> in the app.

## Code mode

Code mode is a full coding-agent workspace around the engines you already use.
Tidebreak manages the checkout, sessions, review, pull request, CI, and delivery
workflow while the selected engine writes the code. Conversations, tool calls,
approvals, usage, changes, and failures remain visible as structured workspace
state.

| Area | Capabilities |
| --- | --- |
| **Engines and models** | Run Claude Code, Codex CLI, opencode, or Grok CLI. Tidebreak downloads the supported engine the first time you select it, uses the engine's own sign-in, and exposes the models, reasoning levels, fast modes, and permission modes that engine supports. |
| **Isolated workspaces** | Register a local checkout, or clone from a Git URL or GitHub. Each workspace gets its own branch and Git worktree, so agents can work in parallel without touching your working copy. Configure setup and archive scripts, keep quick actions per repository, and restore archived work later. With Model Gateway, the same workspace can run on a hosted machine that clones, commits, pushes, and works with pull requests as the connected person. |
| **Sessions and control** | Keep several conversations in one workspace and run separate workspaces side by side. Fork any completed turn into a new conversation or workspace. Inspect supported engine subagents as child rows. Attach files and images, queue follow-ups, steer a live turn, stop work, and change the model or reasoning effort between turns. Shared-worktree turns run in sequence, so two engines never edit the same checkout at once. |
| **Coding workbench** | Browse files, inspect the whole diff or one turn, and open the worktree in your editor. Keep several terminal and browser tabs beside the conversation, split them into panes, test responsive pages, inspect elements, and capture screenshots. Supported agents receive scoped browser tools instead of control over your everyday browser. |
| **Source control and review** | Review unified diffs, then commit and push from the source-control view. Create or update a pull request, move a draft into review, read checks and comments, see stacked pull requests as lanes, update a stale branch, resolve conflicts, and choose the supported merge method or auto-merge. Merge and auto-merge always start with a user action. |
| **CI repair and automation** | Give the agent failing job logs, review feedback, conflicts, or a stale branch to repair. **Watch and fix** survives app restarts, handles new failures, and stops when the pull request is ready for you. Durable triggers can react to failed checks, conflicts, requested changes, stale branches, ready-to-merge state, pull request updates, merges, and closures. |
| **Pull requests and navigation** | Track pull requests, workflow runs, and deployments across repositories in **Pull requests**. Follow changes in **Notifications**, and find old work in **Archive**. The command palette reaches workspaces, files, actions, review, terminals, Pull requests, Analytics, settings, and navigation. |
| **Analytics and health** | See sessions, turn outcomes, context use, tokens, prompt-cache traffic, model and engine breakdowns, pull requests opened and merged, estimated cost when pricing is known, and subscription usage. The coding-engine doctor reports installation, authentication, version, and capability problems. |

Tidebreak is not an integrated development environment (IDE). Keep your editor
for hands-on coding. Use Tidebreak to coordinate agents, isolate their work,
review what they changed, and carry the result through CI and pull request
delivery.

The [Code mode guide](https://www.tidebreak.io/docs/code-mode/) covers engines,
repositories, workspaces, permissions, review, and pull request delivery.

## Work mode

Work mode starts with your files and ends with a deliverable you can inspect,
restore, and export. The agent can research, analyze, write, calculate, run
code, delegate bounded jobs, and build applications while you keep one durable
conversation around the work.

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/tidebreak-work-dark.webp">
    <img src="assets/tidebreak-work-light.webp" alt="Tidebreak in Work mode coordinating four background agents to build a retail launch model, with the finished spreadsheet open beside the conversation">
  </picture>
</p>

| Area | Capabilities |
| --- | --- |
| **Files and sources** | Attach documents, images, and text, or connect a folder through the operating-system picker. Preview PDF, Word, PowerPoint, spreadsheets, images, and text in the app. Source links jump to the page, line, sheet, or cell that supports an answer. |
| **Deliverables** | Files written to `output/` become versioned outputs. Preview any version, restore without deleting later history, export the version you want, and keep Plotly charts interactive. Write back into a connected folder; replacing an existing file always asks. |
| **Research and browsing** | Search with Anthropic or OpenAI built-in search, Exa, Tavily, Brave, Firecrawl, or self-hosted SearXNG. Fetch pages as sources. Use the embedded browser for visible web work and computer use for consent-gated screen capture and desktop app control. |
| **Agents and durable work** | Delegate background jobs to sandboxed agents and inspect their progress and files. Queue follow-ups or steer the live turn. Turns, approvals, user questions, queued work, and the unified inbox survive restarts. |
| **Documents and data** | Use bundled skills for Word, PDF, PowerPoint, spreadsheets, and charts. The agent can clean data, build financial models, write reports, create decks, generate PDFs, and produce working web apps. |
| **Apps and integrations** | Add your own skills and plugins. Connect Model Context Protocol (MCP) servers over standard input/output or HTTP. Register REST APIs from an OpenAPI document. Ask the agent to create a local mini-app and keep it in the sidebar. |
| **Voice and interaction** | Dictate with local or cloud speech recognition. Attach files and folders with `@`, attach skills and saved prompts with `/`, answer structured questions in place, and review tool activity while the turn runs. |

## Models, execution, and permissions

| Layer | Choices |
| --- | --- |
| **Models** | ChatGPT Plus or Pro; Anthropic, OpenAI, Gemini, xAI, Fireworks, Together, or OpenRouter API keys; a local or remote OpenAI-compatible endpoint, including Ollama. Switch providers during a conversation while Tidebreak keeps one provider-neutral history. |
| **Execution** | macOS sandbox, local Docker, E2B, or Daytona for generated code and background work. Choose the network policy per conversation. Code workspaces use local or optional Model Gateway-hosted machines. |
| **Permissions** | Plan, Ask, Auto, or Allow all, with the exact choices limited to what the selected runtime or coding engine can enforce. Folder grants belong to the conversation. Standing grants are visible and revocable. |
| **Extensions** | Built-in and personal skills, plugins, MCP servers and apps, OpenAPI-backed connected apps, local apps, web search, browser tools, and computer use. |
| **Interfaces** | Tauri desktop app, headless server, command-line client, local HTTP and WebSocket API, and an MCP server surface. The desktop and headless clients share the same Rust runtime. |

The [website](https://www.tidebreak.io) gives the product overview. The
[documentation](https://www.tidebreak.io/docs/) covers installation,
providers, permissions, connected folders, execution, outputs, extensions,
Code mode, and the headless interface.

## Common questions

### Does Tidebreak replace Claude Code or Codex CLI?

No. Tidebreak runs Claude Code, Codex CLI, opencode, or Grok CLI and adds the
workspace around them: isolated worktrees, durable sessions, review, CI repair,
pull requests, delivery, and analytics.

### Do I need a Tidebreak account?

No. Use a ChatGPT Plus or Pro subscription, a provider API key, Ollama, or
another OpenAI-compatible endpoint. Anthropic connects through an API key;
Tidebreak does not use Claude subscription sign-in.

### Can Tidebreak run locally and offline?

The app, conversations, outputs, and workspace state live on your machine. To
keep inference and execution local, use Ollama, a local execution backend, no
external integrations, and an offline network policy.

### Is Tidebreak free and open source?

Yes. Tidebreak uses the Apache-2.0 license. You pay the model, search, or hosted
execution providers that you choose; the Tidebreak desktop adds no usage fee.

## Downloads

| Platform | Packages | Notes |
| --- | --- | --- |
| **macOS** | [Universal `.dmg`](https://github.com/brightwave-inc/tidebreak/releases/latest/download/Tidebreak-macos-universal.dmg) | Apple Silicon and Intel; signed and notarized |
| **Windows** | [x86_64 installer](https://github.com/brightwave-inc/tidebreak/releases/latest/download/Tidebreak-windows-x86_64-setup.exe) · [ARM64 installer](https://github.com/brightwave-inc/tidebreak/releases/latest/download/Tidebreak-windows-aarch64-setup.exe) | Windows may show a SmartScreen warning while installers are not Authenticode-signed |
| **Linux** | x86_64 [AppImage](https://github.com/brightwave-inc/tidebreak/releases/latest/download/Tidebreak-linux-x86_64.AppImage) / [`.deb`](https://github.com/brightwave-inc/tidebreak/releases/latest/download/Tidebreak-linux-x86_64.deb) · ARM64 [AppImage](https://github.com/brightwave-inc/tidebreak/releases/latest/download/Tidebreak-linux-aarch64.AppImage) / [`.deb`](https://github.com/brightwave-inc/tidebreak/releases/latest/download/Tidebreak-linux-aarch64.deb) | Built on Ubuntu 22.04; use a compatible glibc-based distribution |

Every package has a `.sha256` sidecar on the release. See the
[installation guide](https://www.tidebreak.io/docs/installation/) for checksum
commands, Linux runtime requirements, updates, and platform limits.

## Quick start

1. Install a desktop package and open Tidebreak.
2. In **Settings → Providers**, sign in with ChatGPT Plus or Pro, add a provider
   API key, or configure Ollama or another OpenAI-compatible endpoint.
3. To create a deliverable, open **Work**, attach a file or connect a folder,
   and ask for the spreadsheet, deck, report, chart, or app you need.
4. To change software, open **Code**, add or clone a repository, choose a coding
   engine, and describe the change. If the pinned engine is missing, Tidebreak
   downloads it. Sign in through the engine's own terminal flow when prompted.
5. Review the output. Export a file version from **Work**, or inspect the diff,
   checks, and review state before you merge from **Code**.

The [full quickstart](https://www.tidebreak.io/docs/quickstart/) explains a
first file-backed task and the approval prompts you will see.

## Data, privacy, and security

The desktop runs on your machine and does not require a Tidebreak account. It
stores chats, attached documents, outputs, code history, and settings in the
local app profile. Provider and integration credentials live in the operating
system's credential store. Code worktrees live under a visible root that you
control.

Local-first does not mean offline. A cloud model receives the prompt and source
content needed for the turn. Search providers receive queries. Connected APIs,
MCP servers, managed sandboxes, and hosted machines receive the calls or work
that you send to them. To keep a workflow on the machine, use a local model,
local execution, no external integrations, and an offline network policy.

Permission modes govern what an agent may do without asking. The selected
execution backend and network policy govern where code runs and what it can
reach. Overwriting a connected file always asks. In Code mode, the agent can
prepare and repair a pull request, but only you invoke merge.

Start with the
[permission guide](https://www.tidebreak.io/docs/permission-modes/) and
[code-execution guide](https://www.tidebreak.io/docs/code-execution/). Report
vulnerabilities privately through [`SECURITY.md`](SECURITY.md), not a public
issue.

## Run from source

Install [rustup](https://rustup.rs/) for the pinned Rust toolchain, Node.js 22,
[pnpm](https://pnpm.io), the
[Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/), the Tauri
CLI, and CMake for the local voice engine:

```sh
cargo install tauri-cli --version "^2"
```

On macOS or Linux, run the development script from the repository root:

```sh
scripts/dev.sh
```

The script checks prerequisites, installs the desktop UI dependencies, and
opens the app. On Windows, or without Bash, run the equivalent commands:

```sh
cd crates/tidebreak-desktop
pnpm --dir ui install
cargo tauri dev
```

See [`CONTRIBUTING.md`](CONTRIBUTING.md#development) for formatting, tests, and
platform notes. See
[`crates/tidebreak-desktop/README.md`](crates/tidebreak-desktop/README.md) for
the native and browser-based UI workflows.

The headless server uses the same core runtime:

```sh
ANTHROPIC_API_KEY=sk-... cargo run -p tidebreak-cli -- serve
```

See the [headless documentation](https://www.tidebreak.io/docs/headless/) for
one-shot mode, the HTTP API, MCP server, configuration, and self-hosting.

## Repository guide

| Path | Purpose |
| --- | --- |
| [`crates/`](crates) | Rust workspace: agent runtime, providers, server, CLI, sandboxing, MCP, coding harnesses, and desktop host |
| [`crates/tidebreak-desktop/ui/`](crates/tidebreak-desktop/ui) | React frontend embedded by the Tauri desktop app |
| [`docs-site/`](docs-site) | Source for the public user documentation |
| [`docs/`](docs) | Maintainer architecture, contracts, operations, and decision records |
| [`skills/`](skills) and [`plugins/`](plugins) | Bundled artifact skills and plugin manifests |
| [`deploy/self-host/`](deploy/self-host) | Self-host deployment assets |

Start with [`docs/how-tidebreak-works.md`](docs/how-tidebreak-works.md) for an
end-to-end technical tour, [`docs/code-mode.md`](docs/code-mode.md) for the
coding architecture, or [`docs/crates.md`](docs/crates.md) for the workspace
map. [`docs/README.md`](docs/README.md) indexes the maintainer documentation.

## Contributing

Tidebreak is built in the open. Bug reports, focused fixes, and design
discussion are welcome. Read [`CONTRIBUTING.md`](CONTRIBUTING.md) before
opening a substantial change. Decisions that future work must preserve live in
[`docs/decisions/`](docs/decisions). The
[`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md) covers all participation.

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

Tidebreak is licensed under Apache-2.0. See [`LICENSE`](LICENSE),
[`NOTICE`](NOTICE), and the
[contributor license agreement](CONTRIBUTING.md#contributor-license-agreement).
