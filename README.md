<p align="center">
  <br>
  <img src="assets/tidebreak-mark.svg" alt="Tidebreak mark" width="300">
</p>

<h1 align="center">Tidebreak</h1>

<p align="center">
  <strong>A private AI coworker that runs on your machine.</strong><br>
  Bring your files and model, then leave with versioned documents, analysis,
  and reusable local apps.
</p>

<p align="center">
  <a href="https://www.tidebreak.sh">Website</a> ·
  <a href="https://www.tidebreak.sh/docs/">User documentation</a> ·
  <a href="https://github.com/brightwave-inc/tidebreak/releases/latest">Latest release</a> ·
  <a href="CONTRIBUTING.md">Contributing</a>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License: Apache-2.0">
  <img src="https://img.shields.io/badge/status-pre--1.0-orange.svg" alt="Status: pre-1.0">
  <img src="https://img.shields.io/badge/built%20with-Rust-dea584.svg?logo=rust&logoColor=white" alt="Built with Rust">
  <a href="https://github.com/brightwave-inc/tidebreak/releases/latest"><img src="https://img.shields.io/github/v/release/brightwave-inc/tidebreak?label=release&color=informational" alt="Latest release"></a>
</p>

<p align="center">
  <a href="https://github.com/brightwave-inc/tidebreak/releases/latest/download/Tidebreak-macos-universal.dmg"><img src="https://img.shields.io/badge/Download%20for%20macOS-000000.svg?logo=apple&logoColor=white" alt="Download the latest macOS release"></a>
</p>

---

Tidebreak is a local-first desktop agent for work that ends in a file, not just
a chat response. It can read attached documents, work in explicitly connected
folders, run code in a sandbox, search the web, delegate parallel jobs, and
publish anything it creates as a versioned output.

You choose the model route: Anthropic, OpenAI, Google, xAI, OpenRouter, a local
Ollama daemon, or another OpenAI-compatible endpoint. Conversations can switch
providers without starting over. Credentials stay in the operating system's
credential store, while chats and outputs remain in the local Tidebreak
profile.

The [website](https://www.tidebreak.sh) is the product overview. The
[user documentation](https://www.tidebreak.sh/docs/) covers installation,
configuration, permissions, connected folders, code execution, outputs, and
the headless CLI.

> [!WARNING]
> Tidebreak is pre-1.0 and changing quickly. Interfaces and local data formats
> may change between releases, and profiles may be rebuilt rather than
> migrated. Packaged releases are currently macOS-only; Windows and Linux can
> be built from source.

## Run from source

Install the pinned Rust toolchain, [pnpm](https://pnpm.io), the
[Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/), and CMake
(used by the local voice engine). Then, from the repository root:

```sh
scripts/dev.sh
```

That installs the desktop UI dependencies and opens the app. See
[`CONTRIBUTING.md`](CONTRIBUTING.md#development) for formatting, tests, and
platform notes, or
[`crates/tidebreak-desktop/README.md`](crates/tidebreak-desktop/README.md) for
the native and browser-based UI workflows.

The headless server uses the same core runtime:

```sh
ANTHROPIC_API_KEY=sk-... cargo run -p tidebreak-cli -- serve
```

See the [headless documentation](https://www.tidebreak.sh/docs/headless/) for
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
[`docs/decisions/`](docs/decisions).

## License

Apache-2.0. See [`LICENSE`](LICENSE), [`NOTICE`](NOTICE), and the
[contributor license agreement](CONTRIBUTING.md#contributor-license-agreement).
