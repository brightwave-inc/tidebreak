# The OpenWave crates

OpenWave is a single Cargo workspace. It splits into **libraries** (reusable, and
some independently publishable) and **client binaries** (the apps you run). The
one rule that keeps it clean: **dependencies only flow downward toward
`openwave-core`** — a library never depends on a client, and clients compose
libraries.

```
      clients            openwave-desktop   openwave-cli
                               │                 │
                               └────── openwave-server
                                            │
      libraries          openwave-mcp
                         openwave-router
                         openwave-host-broker  openwave-code-execution
                         openwave-egress       openwave-sandbox-protocol
                                            │
      the seam                         openwave-core

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

Owns the concrete Anthropic, OpenAI Responses, xAI Responses, Gemini, Google
Vertex AI, Amazon Bedrock Mantle, and OpenAI-compatible provider adapters
(including Fireworks, Together, OpenRouter, vLLM, and LM Studio endpoints)
plus a composite `Router`. Vertex keeps native Gemini GenerateContent and
Claude Messages-over-`streamRawPredict` as separate protocol families under
one explicit provider identity.

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

**Depends on:** `openwave-core`.

---


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
only opaque summaries. Foreground connected-folder tools and the desktop-only
sandbox read of one exact delegated file use this operation boundary; private
scratch tools remain separate and confined to app storage.
See [Host access and connected folders](host-access.md).

**Depends on:** no OpenWave client crate.

## `openwave-sandbox-protocol` — the sandbox-agent wire protocol 🟡

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

**Depends on:** no OpenWave crate (standalone wire contract).


## `openwave-code-execution` — provider-neutral command execution 🟢

The stable `exec` tool, normalized request/result contract, and native local
sandbox. Requests carry a canonical execution id for retry reconciliation and
an opaque workspace id that providers interpret without exposing a host path to
the model.

The initial local provider is macOS Seatbelt: no direct network, one exact
loopback broker pinhole when a chat grants egress, no inherited environment or
stdin, writes confined to private chat scratch, bounded time and output,
process-group cleanup, and private running/terminal receipts. Other platforms
fail closed rather than running unconfined. `openwave-server` owns the runtime
provider/timeout setting and resolves the same per-chat network policy into the
local broker, E2B, or Daytona behind the common contract.

See [Code execution](code-execution.md).

**Depends on:** `openwave-core`, `openwave-egress`.

## `openwave-egress` — egress policy decisions 🟢

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

## `openwave-mcp` — the MCP face 🟡

The server half of [MCP](https://modelcontextprotocol.io): JSON-RPC
`initialize`, `ping`, `tools/list`, and `tools/call` over stdio, backed by
OpenWave's tool registry. Its atomic session lifecycle gates normal operations,
and its execution boundary exposes read-only tools by default; wiring in an
approval gate additionally exposes Workspace and Sensitive tools, routing each
mutating `tools/call` through the same gate and standing grants the in-app agent
consults. Its client
half initializes external stdio servers, follows paginated tool discovery, and
mounts each proxy as `mcp__{server}__{tool}` in the same registry. Mounted tools
are classified sensitive so they cross OpenWave's approval boundary before the
client forwards a call. Their generic approval is one-shot rather than a
standing name-based grant, since Settings can replace the executable behind a
stable namespace. The desktop Settings page owns typed runtime configuration
and renderer-safe health, while `OPENWAVE_MCP_CONFIG` remains a headless
bootstrap path. The server supervises idle-session health with bounded
reconnect backoff and refreshes changed tool lists by publishing a fresh
immutable registry for subsequent turns.

**Depends on:** `openwave-core`.

---

## `openwave-desktop` — the desktop app 🟡

The Tauri application: it compiles the server in-process, hosts the chat UI, and
talks to it over an ephemeral loopback HTTP/WebSocket surface. This is the
primary way most people will run OpenWave. Its private native executor also
recovers exact delegated-file checkpoints, revalidates product attachment
authority, and sends one bounded read through the host broker without exposing
the target or executor credentials to the renderer. See
[`crates/openwave-desktop/README.md`](../crates/openwave-desktop/README.md) for
local run instructions.

**Depends on:** `openwave-core`, `openwave-host-broker`, `openwave-server` (+ Tauri).

## `openwave-server` — local API and workers 🟢

The authenticated loopback HTTP/WebSocket surface shared by desktop and
headless clients. It owns route orchestration and the durable document,
retirement, and audit workers while core state transitions remain in
`openwave-core`.

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

**Depends on:** `openwave-core`, `openwave-router`.

## `openwave-cli` — headless daemon + CLI 🟡

The working headless daemon (`openwave serve`) over the same HTTP surface the
desktop uses, plus `openwave mcp <workspace>` for a read-only MCP stdio server
confined to one explicit workspace. Indexed-document MCP search and additional
command-line client workflows remain in development.

**Depends on:** `openwave-core`, `openwave-mcp`, `openwave-server`.
