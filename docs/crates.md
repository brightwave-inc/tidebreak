# The Tidebreak crates

Tidebreak is a single Cargo workspace. It splits into **libraries** (reusable, and
some independently publishable) and **client binaries** (the apps you run). The
one rule that keeps it clean: **dependencies only flow downward toward
`tidebreak-core`** — a library never depends on a client, and clients compose
libraries.

```
      clients            tidebreak-desktop   tidebreak-cli
                         tidebreak-sandbox-agent
                         tidebreak-supervised-agent
                               │
                         tidebreak-server (crate tidebreak-server-api)
                               │
                         tidebreak-server-core (crate tidebreak-server)
                               │
      libraries          tidebreak-mcp            tidebreak-harness
                         tidebreak-router         tidebreak-code-remote
                         tidebreak-host-broker    tidebreak-code-delivery
                         tidebreak-code-execution tidebreak-sandbox-runtime
                         tidebreak-egress         tidebreak-sandbox-protocol
                         tidebreak-gateway-runtime
                         tidebreak-worker-runtime
                         tidebreak-managed-node
                         tidebreak-shell-policy
                                            │
      the seam                         tidebreak-core

```

`tidebreak-whisper` is its own Cargo workspace, excluded from the root one.

**Status legend:** 🟢 built/in active development · 🟡 partial baseline · ⚪ stub.

---

## `tidebreak-core` — the open-core seam 🟢

The foundation every client, including a future managed control plane, sits on.
It's independently publishable on crates.io and **never depends on a specific
client**.

It holds the agent loop, the tool registry, the `AgentEvent` stream that every
client renders from, and the trait **contracts** — `Tool`, `ModelProvider`,
`Store`, `BlobStore`, `SecretProvider` — together with their default local
implementations (SQLite, the local filesystem, the OS keychain). Concrete
model-provider adapters do **not** live here; they live in `tidebreak-router`.

Major surfaces present today:

| Module | What it is |
| --- | --- |
| `id` | Typed identifiers (`SessionId`, `TurnId`, `CallId`, …) — newtypes so the compiler stops you mixing them up. |
| `error` | The crate-wide `AgentError` + `Result`. |
| `model` | Persisted chats, projects, documents, jobs, tool executions, leases, and lifecycle state. |
| `tool` | The tool contract (`Tool`, `ToolSpec`, `ToolOutput`, `ToolCtx`, `ApprovalClass`). |
| `provider` | The model-provider contract (`ModelProvider`, `ChatRequest`, `ProviderEvent`, `Usage`). |
| `agent` | The cancellable/steerable multi-step turn loop and durable event journal integration. |
| `db` / `storage` | SQLite/PostgreSQL-capable state transitions plus in-memory implementations, including immutable accept/lease/terminal tool execution. |
| `blob` / `keychain` | Immutable local blob storage and OS-backed secret storage. |

**Depends on:** nothing in the workspace.

---

## `tidebreak-router` — model providers & routing 🟢

Owns the concrete Anthropic, OpenAI Responses, xAI Responses, Gemini, and
OpenAI-compatible provider adapters (including Fireworks, Together, OpenRouter,
Ollama, vLLM, and LM Studio endpoints) plus a composite `Router`.

The `Router` is itself a `ModelProvider`, so the agent loop holds one provider
contract and does not depend on a concrete backend. Two things define it:

- **Embedded local routing.** Provider credentials remain on the device and the
  selected model determines the adapter used for each request.
- **No default provider = fail-closed egress.** It calls no model until one is
  explicitly configured *and* enabled — nothing leaves your machine by accident.

Health-based failover remains planned.

Capability beyond plain chat is tiered: see
[Model providers and cross-provider replay](model-providers.md). Advanced
features are designed against Tier-1 providers; other routes stay honestly
partial via registry flags, and foreign native artifacts flatten on switch
instead of growing an N² translation layer.

**Depends on:** `tidebreak-core`.

---


## `tidebreak-host-broker` — consented host access 🟡

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
only opaque summaries. Foreground connected-folder tools and the desktop-only
sandbox read of one exact delegated file use this operation boundary; private
scratch tools remain separate and confined to app storage.
See [Host access and connected folders](host-access.md).

**Depends on:** no Tidebreak client crate.

## `tidebreak-sandbox-protocol` — the sandbox-agent wire protocol 🟡

The versioned boundary between the host and a sandbox-resident agent run —
provisioning, run init, the resumable monotonically sequenced event stream,
artifact collection, and the reverse-RPC callback channel with host-proxied
model inference as its first capability. It is a public interface third parties
implement (a self-hosted backend runs the sandbox side of it), so the wire
contract, not any one backend, is the deliverable. It follows the host-broker
envelope discipline: a `PROTOCOL_VERSION` checked for exact equality with an
attach handshake, deny-by-default capability grants carrying run provenance, a
reserved control lane for cancel/liveness kept off the request lane, and bounded
typed results with explicit per-capability bounds. The provision/address/destroy
decomposition treats a self-hosted backend (no provisioning, just an address and
a credential) as the conformance test rather than a special case. It ships an
in-process reference backend and a conformance suite (the CI artifact), plus the
operation-identity state machine backed by an in-memory store behind a durable
seam. **The protocol is UNSTABLE until a named release.** The crash-safe durable
operation log and its retention are split into focused follow-ups.
See [Execution providers and sandbox-resident agent runs](sandbox-providers.md).

**Depends on:** no Tidebreak crate (standalone wire contract).


## `tidebreak-code-execution` — provider-neutral command execution 🟢

The stable `exec` tool, normalized request/result contract, and native local
sandbox. Requests carry a canonical execution id for retry reconciliation and
an opaque workspace id that providers interpret without exposing a host path to
the model.

The initial local provider is macOS Seatbelt: no direct network, one exact
loopback broker pinhole when a chat grants egress, no inherited environment or
stdin, writes confined to private chat scratch, bounded time and output,
process-group cleanup, and private running/terminal receipts. Other platforms
fail closed rather than running unconfined. `tidebreak-server` owns the runtime
provider/timeout setting and resolves the same per-chat network policy into the
local broker, E2B, Daytona, or — for its strictest class only — a container
created with no network at all.

See [Code execution](code-execution.md).

**Depends on:** `tidebreak-core`, `tidebreak-egress`.

## `tidebreak-code-remote` — remote sandbox control 🟢

The client side of the confining environment's sandbox runtime API. Decision
0079 split remote execution in two: the workload half is
`tidebreak-supervised-agent`; this crate is Tidebreak asking that environment
to provision, watch, steer, and stop the sandbox a remote session's engine
runs in. Tidebreak never dials into the pod. The pinned contract is
`/api/v1/runtime/...` on the gateway (spawn, status, sequenced events, inbox
messages, cancel).

**Depends on:** `tidebreak-core`.

## `tidebreak-code-delivery` — GitHub delivery 🟢

Install-wide GitHub delivery reads and guarded user actions. The database
remains the source of truth for registered repositories, Tidebreak
workspaces, and attributed pull-request facts. Workflow run summaries persist
as `code_workflow_run` rows; deployments stay live GitHub observations in a
short in-memory cache.

**Depends on:** `tidebreak-core`.

## `tidebreak-sandbox-agent` — in-container sandbox agent 🟡

The sandbox-resident side of the sandbox-provider design: a container image
running Tidebreak's agent loop with a closed, sandbox-resident tool registry,
behind `tidebreak-sandbox-protocol`. The supervisor owns the transport
listener; the agent loop drives model inference back to the host over reverse
RPC so no model credential lives in the container; a separate egress proxy
enforces the run's compiled network policy.

**Depends on:** `tidebreak-sandbox-protocol`, `tidebreak-core`.

## `tidebreak-sandbox-runtime` — sandboxed background-agent execution 🟢

Owns in-process sandbox runs, container-hosted runs, Docker lifecycle,
detached-admission checks, exact-attempt cancellation, and the durable
reverse-operation log. The embedding server supplies model routing, live
settings, event publication, and tool catalogs through narrow traits.

**Depends on:** `tidebreak-core`, `tidebreak-sandbox-protocol`.

## `tidebreak-worker-runtime` — durable worker pacing 🟢

Shared pacing and retry contracts for durable workers (lanes and retry).

**Depends on:** nothing in the workspace.

## `tidebreak-supervised-agent` — externally supervised agent 🟡

An externally supervised sandbox — a controlled execution environment that
Tidebreak does not provision — starts this agent, owns the durable event
stream, and exposes a control endpoint. The agent initiates outbound polls to
that endpoint, drives an engine CLI through `tidebreak-harness`, and reports
lifecycle events outward. It runs no listener, accepts no attach, and keeps
no durable state of its own.

**Depends on:** `tidebreak-harness`, `tidebreak-core`.

## `tidebreak-managed-node` — managed Node runtime contract 🟢

Shared verification contract for Tidebreak's managed Node runtime. The
desktop owns downloading and unpacking Node. Consumers only trust the
resulting directory when its marker names the exact artifact pinned for the
current platform and both required entrypoints are present.

**Depends on:** nothing in the workspace.

## `tidebreak-gateway-runtime` — model-gateway session 🟢

Model-gateway connection, session, catalog, and relay runtime. The embedding
server supplies managed policy, model persistence, pairing, and MCP
configuration through narrow traits. The runtime owns the network session,
authority fence, sign-in lifecycle, catalog refresh, endpoint entitlements,
and shared-app relay.

**Depends on:** `tidebreak-core`, `tidebreak-router`.

## `tidebreak-shell-policy` — shell command analysis 🟢

Deterministic safety analysis for shell commands. Given a raw command and
standing allow/deny rules, it decides whether the command may run without
asking, must be put to a human, or is structurally unsafe. The crate is
pure: no process is spawned and no filesystem is touched.

**Depends on:** nothing Tidebreak-specific beyond the parser.

## `tidebreak-harness` — external engine events 🟢

Protocol translation from an external agent engine into one normalized event
vocabulary. Nothing in this crate's traits assumes the engine is a coding
agent. Orchestration, persistence, and UI consume only `tidebreak_core::Event`;
this crate emits the unpersisted sibling `HarnessEvent`.

**Depends on:** `tidebreak-core`.

## `tidebreak-whisper` — on-demand transcription helper 🟢

One-shot whisper.cpp transcription helper. The desktop spawns this binary per
transcription instead of linking whisper.cpp. It is excluded from the root
workspace and published separately.

**Depends on:** whisper.cpp (its own workspace).

## `tidebreak-egress` — egress policy decisions 🟢

The dependency-free decision layer from
[sandbox providers](sandbox-providers.md): one deny-by-default allowlist
policy (wildcard domain patterns and CIDR blocks) answering whether a workload
may open a connection to a destination, consulted by every enforcement point.
It also owns the enforcement-tier vocabulary — external enforcement is a
boundary, supervisor enforcement is defense in depth — and the per-backend
enforcement declaration, stated as what the mechanism actually blocks with
vendor exceptions included, which the admission rule for
third-party-credential-bearing work checks. Deliberately std-only so the
future in-sandbox supervisor can consult the same decision without pulling an
HTTP client or async runtime into the sandbox image.

**Depends on:** nothing in the workspace.

## `tidebreak-mcp` — the MCP face 🟡

The server half of [MCP](https://modelcontextprotocol.io): JSON-RPC
`initialize`, `ping`, `tools/list`, and `tools/call` over stdio, backed by
Tidebreak's tool registry. Its atomic session lifecycle gates normal operations,
and its execution boundary exposes read-only tools by default; wiring in an
approval gate additionally exposes Workspace and Sensitive tools, routing each
mutating `tools/call` through the same gate and standing grants the in-app agent
consults. Its client
half initializes external stdio servers, follows paginated tool discovery, and
mounts each proxy as `mcp__{server}__{tool}` in the same registry. Mounted tools
are classified sensitive so they cross Tidebreak's approval boundary before the
client forwards a call. Their generic approval is one-shot rather than a
standing name-based grant, since Settings can replace the executable behind a
stable namespace. The desktop Settings page owns typed runtime configuration
and renderer-safe health, while `TIDEBREAK_MCP_CONFIG` remains a headless
bootstrap path. The server supervises idle-session health with bounded
reconnect backoff and refreshes changed tool lists by publishing a fresh
immutable registry for subsequent turns.

**Depends on:** `tidebreak-core`.

---

## `tidebreak-desktop` — the desktop app 🟡

The Tauri application: it compiles the server in-process, hosts the chat UI, and
talks to it over an ephemeral loopback HTTP/WebSocket surface. This is the
primary way most people will run Tidebreak. Its private native executor also
recovers exact delegated-file checkpoints, revalidates product attachment
authority, and sends one bounded read through the host broker without exposing
the target or executor credentials to the renderer. See
[`crates/tidebreak-desktop/README.md`](../crates/tidebreak-desktop/README.md) for
local run instructions.

**Depends on:** `tidebreak-core`, `tidebreak-host-broker`, `tidebreak-server` (+ Tauri).

## `tidebreak-server-api` — HTTP and WebSocket routes 🟢

Package name `tidebreak-server`. Tidebreak's in-process HTTP and WebSocket
route surface. Desktop and CLI depend on this crate; it re-exports workers
and tools from `tidebreak-server-core`.

**Depends on:** `tidebreak-server-core`.

## `tidebreak-server` — local API workers 🟢

Directory `crates/tidebreak-server`, package name `tidebreak-server-core`.
The authenticated loopback workers shared by desktop and headless clients.
It owns the durable document, retirement, and audit workers while core state
transitions remain in `tidebreak-core`. HTTP routes live in
`tidebreak-server-api`, not here.

Two former standalone crates now live here as modules, because the server
was their only consumer:

- `connectors` — loopback OAuth (RFC 8252 + PKCE) for model-gateway and
  ChatGPT subscription sign-in, plus token vault helpers.
- `web_search` — provider-neutral search/extract adapters, fetch admission,
  native page extraction, and the host configuration surface that selects
  and credentials them.


Client-owned tool work is exposed through authenticated per-chat polling,
claim, heartbeat, and resolution routes. General records show visible lease
metadata but never the secret claim token; only the claim response returns that
receipt.

The embedded-desktop profile additionally enables the argument-free
`read_delegated_file` checkpoint for a depth-one child with one immutable exact
file delegation. Native-only pending/claim/heartbeat/resolve routes drive it;
the headless profile does not advertise the tool because it has no embedded
executor.

**Depends on:** `tidebreak-core`, `tidebreak-router`.

## `tidebreak-cli` — headless daemon + CLI 🟡

The working headless daemon (`tidebreak serve`) over the same HTTP surface the
desktop uses, plus `tidebreak mcp <workspace>` for a read-only MCP stdio server
confined to one explicit workspace. Indexed-document MCP search and additional
command-line client workflows remain in development.

**Depends on:** `tidebreak-core`, `tidebreak-mcp`, `tidebreak-server`.
