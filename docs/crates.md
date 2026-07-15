# The OpenWave crates

OpenWave is a single Cargo workspace. It splits into **libraries** (reusable, and
some independently publishable) and **client binaries** (the apps you run). The
one rule that keeps it clean: **dependencies only flow downward toward
`openwave-core`** — a library never depends on a client, and clients compose
libraries.

```
      clients            openwave-desktop   openwave-cli   openwave-slack (stub)
                               │                 │
                               └────── openwave-server
                                            │
      libraries          openwave-mcp   openwave-retrieval   openwave-router
                         openwave-web-search   openwave-host-broker │
                               │             │              │      │
                               └─────────────┴──────────────┴──────┘
                                            │
      the seam                         openwave-core

      scaffold                         openwave-connectors (stub)
```

**Status legend:** 🟢 built/in active development · 🟡 partial baseline · ⚪ stub.

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

Major surfaces present today:

| Module | What it is |
| --- | --- |
| `id` | Typed identifiers (`ChatId`, `TurnId`, `CallId`, …) — newtypes so the compiler stops you mixing them up. |
| `error` | The crate-wide `AgentError` + `Result`. |
| `model` | Persisted chats, projects, documents, jobs, tool executions, leases, and lifecycle state. |
| `tool` | The tool contract (`Tool`, `ToolSpec`, `ToolOutput`, `ToolCtx`, `ApprovalClass`). |
| `provider` | The model-provider contract (`ModelProvider`, `ChatRequest`, `ProviderEvent`, `Usage`). |
| `agent` | The cancellable/steerable multi-step turn loop and durable event journal integration. |
| `db` / `storage` | SQLite/PostgreSQL-capable state transitions plus in-memory implementations, including immutable accept/lease/terminal tool execution. |
| `blob` / `keychain` | Immutable local blob storage and OS-backed secret storage. |

**Depends on:** nothing in the workspace.

---

## `openwave-router` — model providers & routing 🟢

Owns the concrete Anthropic and OpenAI-compatible provider adapters (including
OpenAI, Fireworks, OpenRouter, vLLM, and LM Studio endpoints) plus a composite
`Router`.

The `Router` is itself a `ModelProvider`, so the agent loop holds one provider
contract and does not depend on a concrete backend. Two things define it:

- **Embedded local routing.** Provider credentials remain on the device and the
  selected model determines the adapter used for each request.
- **No default provider = fail-closed egress.** It calls no model until one is
  explicitly configured *and* enabled — nothing leaves your machine by accident.

Health-based failover remains planned.

**Depends on:** `openwave-core`.

---

## `openwave-connectors` — OAuth & source connectors ⚪

Scaffold reserved for loopback OAuth (RFC 8252 + PKCE), token refresh, and
connector tools that list and fetch from sources like Drive and Box.

**Depends on:** nothing in the workspace yet.

## `openwave-host-broker` — consented host access 🟡

The runtime-neutral trust boundary for connected local folders. It owns opaque
root/grant/operation identities, validated grant and attachment values, portable
relative paths, descriptor-pinned root policy, and a versioned in-process broker
with separate controller/operator handles. Register/revoke mutations are
idempotent across restart through an atomically published, size-bounded private
registry. Restart revalidates and pins every connected root before making it
available, and ambiguous publication fails closed. List/read operations
reauthorize before releasing bounded results so completed revocation fences
in-flight work. Every control/read attempt writes a bounded, de-sensitized local
audit event naming its result and authorizing grant; synced JSONL rotation bounds
local retention without recording absolute paths or contents. Partial writes
are rolled back before retry, interrupted tails/rotations recover on restart,
and degraded read-tier audit does not withhold the user's existing file access.
A runnable sidecar exposes the same core over bounded, strict JSONL stdio,
resynchronizes after oversized input, and protects its own app-data directory
from ever becoming a connected root. The Tauri host owns its lifecycle and
native folder consent behind narrow pick/list/revoke commands; the renderer sees
only opaque summaries. Existing agent file tools do not use this boundary yet.
See [Host access and connected folders](host-access.md).

**Depends on:** no OpenWave client crate.

## `openwave-retrieval` — parsing, search, citations 🟢

Asynchronous parsing, structural chunking, embeddings, scoped hybrid
lexical+dense search, reranking, and grounded citations behind a `VectorStore`
seam with in-memory and durable embedded LanceDB backends. Durable source
revisions and Parse→Index jobs are coordinated by `openwave-core` and
`openwave-server`.

**Depends on:** `openwave-core`.

## `openwave-web-search` — provider-neutral web search 🟢

The bounded request/result contract and direct HTTP adapters for Exa and
Tavily. It is intentionally separate from both the model loop and the tool
registry: loading a credential or constructing an adapter performs no egress,
and calling search requires an explicit host-owned HTTP client. API keys are
resolved only through `SecretProvider` under fixed provider keys, never from a
model argument or persisted tool-call payload. The optional `http` feature
provides a timeout- and response-size-bounded `reqwest` client; hosts may supply
their own proxy, allow-list, audit, or test client through the same seam.

`openwave-server` owns an explicit disabled-by-default Exa/Tavily selection
and bounded request timeout, but has not connected it to a model-visible tool
or worker. The follow-on slice must attach that host policy to the sandbox
execution boundary and define its outbound-domain policy.

**Depends on:** `openwave-core`.

## `openwave-mcp` — the MCP face 🟡

The server half of [MCP](https://modelcontextprotocol.io): JSON-RPC
`initialize`, `ping`, `tools/list`, and `tools/call` over stdio, backed by
OpenWave's tool registry. Its atomic session lifecycle gates normal operations,
and its execution boundary exposes only tools classified read-only. The client
that mounts external MCP servers remains planned.

**Depends on:** `openwave-core`.

---

## `openwave-desktop` — the desktop app 🟡

The Tauri application: it compiles the server in-process, hosts the chat UI, and
talks to it over an ephemeral loopback HTTP/WebSocket surface. This is the
primary way most people will run OpenWave. See
[`crates/openwave-desktop/README.md`](../crates/openwave-desktop/README.md) for
local run instructions.

**Depends on:** `openwave-core`, `openwave-host-broker`, `openwave-server` (+ Tauri).

## `openwave-server` — local API and workers 🟢

The authenticated loopback HTTP/WebSocket surface shared by desktop and
headless clients. It owns route orchestration and the durable document,
retirement, and audit workers while core state transitions remain in
`openwave-core`.

Client-owned tool work is exposed through authenticated per-chat polling,
claim, heartbeat, and resolution routes. General records show visible lease
metadata but never the secret claim token; only the claim response returns that
receipt.

**Depends on:** `openwave-core`, `openwave-router`, `openwave-retrieval`.

## `openwave-cli` — headless daemon + CLI 🟡

The working headless daemon (`openwave serve`) over the same HTTP surface the
desktop uses, plus `openwave mcp <workspace>` for a read-only MCP stdio server
confined to one explicit workspace. Indexed-document MCP search and additional
command-line client workflows remain in development.

**Depends on:** `openwave-core`, `openwave-mcp`, `openwave-server`.

## `openwave-slack` — the Slack adapter ⚪

A scaffold for a Socket Mode adapter (outbound WebSocket, no inbound ports) that
will drive the agent from a Slack workspace.

**Depends on:** nothing in the workspace yet.
