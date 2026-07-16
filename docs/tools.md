# Tool architecture and roadmap

Tools are the boundary between model decisions and real effects. OpenWave keeps
that boundary small, typed, and explicit: every advertised tool has a stable
name and JSON Schema, an approval class, a bounded result, and a recorded call
identity.

## What exists today

The current foreground agent surface contains nine tools:

| Tool | Purpose | Execution boundary |
| --- | --- | --- |
| `read_file` | Read UTF-8 text from private per-chat scratch | Server, read-only |
| `list_dir` | List a private-scratch directory | Server, read-only |
| `write_file` | Atomically write private-scratch text | Server, workspace |
| `search` | Search locally indexed project/chat documents | Server, read-only |
| `request_folder_access` | Ask the trusted desktop host to connect another folder | Client continuation |
| `list_connected_folders` | List roots already attached to this chat | Native client continuation |
| `list_folder` | List one directory below an attached root | Native client continuation |
| `read_connected_file` | Read bounded UTF-8 text below an attached root | Native client continuation |
| `spawn_sandbox_agent` | Delegate one bounded task and wait for its durable result | Foreground-only durable continuation |

The connected-folder calls are foreground-only. Their arguments contain only
an opaque root ID and a bounded root-relative path; native code recovers the
stored chat context, reauthorizes with the broker, and persists the exact
model-facing result before resolving the parked turn. Requesting access and
using an approved root remain separate operations.

## Core module layout

`openwave-core` owns only generic tool contracts and tools that require no
provider or product adapter. Its built-in filesystem implementation is split by
responsibility:

```text
tools/
├── definitions.rs       model-facing names, descriptions, and JSON Schemas
├── arguments.rs         typed argument decoding
├── private_scratch.rs   confined filesystem primitives and limits
├── read_file.rs         one Tool implementation
├── list_dir.rs          one Tool implementation
├── write_file.rs        one Tool implementation
└── tests.rs             shared confinement and behavior coverage
```

Provider-backed tools do not belong in core. Retrieval, web search, connectors,
and sandbox execution should each supply tools from their owning crate behind
the common `Tool` interface.

## Foreground and sandbox surfaces

Foreground and background agents need different tools. OpenWave should not
advertise every installed integration to every model call.

The foreground coordinator needs tools for:

- local corpus search plus document list/read;
- web search and page retrieval;
- connected-root list/read/import and explicit export;
- asking the user a structured question;
- spawning, inspecting, messaging, waiting for, cancelling, and reviewing
  depth-one sandbox agents.

`spawn_sandbox_agent` is enabled only for a claimed foreground turn. It
atomically admits a depth-one child and parks that turn; the child result is
persisted as a system transcript message before the parent can resume. Sandbox
agents never receive this tool.

A background sandbox has no shared conversation, filesystem, network, or
host-folder access. It receives one bounded task and may be offered exactly
one fixed `web_search` contract when its remaining model-step budget can also
consume the result. The worker atomically parks the sandbox under its exact
lease; a separate host-owned executor performs the bounded search and writes
an immutable receipt. On a later claim, the sandbox reconstructs the matching
`ToolUse`/`ToolResult` from that receipt before it can finalize. Sandboxes do
not receive `spawn_sandbox_agent` and cannot create further agents.

Future sandbox-safe capabilities, such as broker-mediated folder reads, must
be added one at a time behind the same durable continuation and consent
boundaries.

Later additions may include a scratchpad, pinned context, plan/execution modes,
deliverable export, clipboard operations, generated images, and connected apps.
Office-suite automation, general computer control, scheduled tasks, enterprise
database tools, and recursive fleets remain deferred.

## Web search

Web search now has a provider-neutral `openwave-web-search` crate rather than
HTTP code embedded in the core loop:

```text
WebSearchProvider
├── ExaProvider
└── TavilyProvider

WebSearchService
├── timeout and retry policy
├── provider selection and health
├── normalized result ranking
└── URL deduplication

WebSearchTool
├── model-facing schema
├── credential-free request
├── bounded output and citations
└── optional durable web-document ingestion
```

The first slice supplies the normalized bounded contract, fixed secret keys,
and direct Exa/Tavily adapters through an injected HTTP seam. The server now
also owns a disabled-by-default provider selection and a 1–60 second request
timeout at `GET`/`PUT /web-search`. That setting has no endpoint or credential
reference, returns only whether the selected fixed key is present, and does not
construct or call a provider. It does **not** register `WebSearchTool` or grant
any worker network access. Constructing an adapter is inert; only an explicit
`search` call can send a request. A separate sandbox checkpoint executor now
attaches the host policy only after claiming and revalidating an exact durable
`web_search` checkpoint. Its strict argument contract rejects unknown fields,
and unavailable/error cases resolve a bounded immutable failure receipt. The
sandbox model loop may now emit the one fixed checkpoint, with a bounded
argument collector and deterministic rejection of unknown, partial, or
multiple calls. The foreground loop remains separate and does not receive this
tool. Outbound-domain policy is the next hardening slice before broadening the
search surface.

The normalized contract should cover query text, optional date/domain filters,
bounded result count, canonical URL, title, snippet or extracted text, rank or
score, publication date, optional image URL, provider metadata, and structured
retryability.

Exa and Tavily both have direct HTTP APIs, so vendor Rust SDKs are unnecessary.
Adapters can use the workspace's existing `reqwest`, `serde`, `tokio`, `chrono`,
`futures`, `uuid`, and hashing dependencies, plus strict URL parsing. API keys
remain provider-specific references in the existing `SecretProvider`; they are
never model arguments or persisted tool-call payloads.

Network tools are sensitive. Approval, allowed outbound domains, credential
injection, cancellation, and timeouts are enforced outside the model-supplied
arguments.

## Reliability rules

Every new tool should answer these questions before it is registered:

1. Is its call identity stable across an ambiguous retry?
2. Is execution server-owned, sandbox-owned, or client-owned?
3. Can it produce an external side effect, and where is the terminal receipt?
4. Can cancellation prove that execution quiesced?
5. Is it safe to run in parallel with sibling calls?
6. Are arguments, output bytes, item counts, and runtime bounded?
7. Can a stale worker publish its result?
8. Does reconnect recovery reconstruct pending user interaction?

Until an effect has an idempotency or reconciliation contract, OpenWave fails
conservatively after an ambiguous execution instead of replaying it.
