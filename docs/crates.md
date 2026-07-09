# The OpenWave crates

OpenWave is a single Cargo workspace. It splits into **libraries** (reusable, and
some independently publishable) and **client binaries** (the apps you run). The
one rule that keeps it clean: **dependencies only flow downward toward
`openwave-core`** — a library never depends on a client, and clients compose
libraries.

```
      client binaries        openwave-desktop   openwave-cli   openwave-slack
                                     │               │              │
                                     └───────────────┼──────────────┘
      libraries                    openwave-mcp  openwave-connectors  openwave-retrieval  openwave-router
                                     └───────────────┴──────────────┴──────────────┘
      the seam                                     openwave-core
```

**Status legend:** 🟢 in progress · 🔵 planned · ⚪ stub (scaffold only).

---

## `openwave-core` — the open-core seam 🟢

The foundation every client (and, later, the Brightwave Connect control plane)
sits on. It's independently publishable on crates.io and **never depends on a
specific client**.

It holds the agent loop, the tool registry, the `AgentEvent` stream that every
client renders from, and the trait **contracts** — `Tool`, `ModelProvider`,
`Store`, `BlobStore`, `SecretProvider` — together with their default local
implementations (SQLite, the local filesystem, the OS keychain). Concrete
model-provider adapters do **not** live here; they live in `openwave-router`.

Present today:

| Module | What it is |
| --- | --- |
| `id` | Typed identifiers (`ChatId`, `TurnId`, `CallId`, …) — newtypes so the compiler stops you mixing them up. |
| `error` | The crate-wide `AgentError` + `Result`. |
| `model` | The persisted conversation model (`Chat`, `Message`, `Role`). |
| `tool` | The tool contract (`Tool`, `ToolSpec`, `ToolOutput`, `ToolCtx`, `ApprovalClass`). |
| `provider` | The model-provider contract (`ModelProvider`, `ChatRequest`, `ProviderEvent`, `Usage`). |

**Depends on:** nothing in the workspace.

---

## `openwave-router` — model providers & routing 🔵

Owns the concrete model-provider adapters (Anthropic, OpenAI, an
OpenAI-compatible adapter that covers Fireworks / OpenRouter / vLLM / LM Studio,
and more) plus a composite `Router`.

The `Router` is itself a `ModelProvider`, so the agent loop just holds one
provider and never knows whether it's a single backend, a failover pool, or a
remote gateway. Two things define it:

- **Two deployment modes, one crate.** It runs **embedded** (local-first, keys on
  the device) or as a **central gateway service** (keys stay server-side; clients
  and sandboxes call it over the network via a thin `RemoteProvider`).
- **No default provider = fail-closed egress.** It calls no model until one is
  explicitly configured *and* enabled — nothing leaves your machine by accident.

Health-based failover (circuit breaker + retry) rounds it out.

**Depends on:** `openwave-core`.

---

## `openwave-connectors` — OAuth & source connectors ⚪

Loopback OAuth (RFC 8252 + PKCE), token refresh, and the connector tools that
list and fetch from sources like Drive and Box.

**Depends on:** `openwave-core`.

## `openwave-retrieval` — embeddings, vector search, citations ⚪

Embeddings, chunking, ingestion, and grounded citations behind a `VectorStore`
seam that runs embedded (sqlite-vec) or against pgvector / Qdrant.

**Depends on:** `openwave-core`.

## `openwave-mcp` — the MCP face ⚪

Both halves of [MCP](https://modelcontextprotocol.io): a **server** that exposes
OpenWave's core tools to external agents (Claude Code, Codex, Cursor…), and a
**client** that mounts external MCP tool servers into the agent, namespaced.

**Depends on:** `openwave-core`.

---

## `openwave-desktop` — the desktop app 🟡

The Tauri application: it compiles the server in-process, hosts the chat UI, and
talks to it over an ephemeral loopback HTTP/WebSocket surface. This is the
primary way most people will run OpenWave. See
[`crates/openwave-desktop/README.md`](../crates/openwave-desktop/README.md) for
local run instructions.

**Depends on:** `openwave-core`, `openwave-server` (+ Tauri).

## `openwave-cli` — headless daemon + CLI ⚪

The headless daemon (`openwave serve`) and a command-line client, over the same
HTTP surface the desktop uses. For servers, scripts, and power users.

**Depends on:** `openwave-core`.

## `openwave-slack` — the Slack adapter ⚪

A Socket Mode adapter (outbound WebSocket, no inbound ports) that drives the
agent from a Slack workspace.

**Depends on:** `openwave-core`.
