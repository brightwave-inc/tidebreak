# Tool architecture and roadmap

Tools are the boundary between model decisions and real effects. OpenWave keeps
that boundary small, typed, and explicit: every advertised tool has a stable
name and JSON Schema, an approval class, a bounded result, and a recorded call
identity.

## What exists today

The current foreground agent surface contains ten tools:

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
| `spawn_sandbox_agent` | Start one isolated background task and immediately continue | Foreground-only durable checkpoint |
| `wait_for_agents` | Wait for one to four spawned agents and return their results in request order | Foreground-only durable continuation |

The two background-agent tools are a closed pair. A spawn returns only an opaque
`agent_id`; a wait accepts one to four unique IDs returned by spawns from the
same foreground turn. Both calls must be made alone, without assistant text or
sibling tool calls. Sandbox agents receive neither definition, so they cannot
create or wait on other agents.

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
- spawning and waiting for depth-one sandbox agents.

`spawn_sandbox_agent` and `wait_for_agents` are enabled only for a durably
claimed foreground turn. A spawn atomically admits the depth-one child,
records the completed tool result containing its `agent_id`, journals that
completion, applies model usage once, and moves the foreground turn to
`resuming`. A fresh worker claim then continues the conversation while the
child runs independently. The notification that wakes each worker is only a
latency hint; the committed state is sufficient after a restart.

One foreground turn may have at most four unsettled children. A child remains
unsettled while it is still running or while its terminal delivery is waiting
to be consumed. The model can launch independent tasks one at a time, retaining
each returned ID, and then make one ordered `wait_for_agents` call. That call
uses `All` semantics: it atomically records the pending tool call and exact
ordered child set, applies progress once, and releases the foreground lease.
After every child has a terminal delivery, recovery consumes all deliveries,
completes the same tool call with results in request order, journals the exact
completion event, and moves the turn to `resuming`.

The foreground turn cannot silently finish while one of its children is still
unsettled. If the model tries, the completion boundary keeps the turn alive and
gives the next model call the complete ordered ID list it must wait for. This is
a storage-backed correctness guard, not merely a prompt convention.

All orchestration identities are stable across ambiguous responses. An exact
spawn or wait retry recovers its prior receipt rather than creating another
child or consuming a result twice. Live event delivery uses the exact journaled
sequence; reconnecting clients replay the journal, and cursor-based consumers
can ignore a repeated live publication after commit-response loss.

A background sandbox has no shared conversation, general filesystem, general
network, or broad host-folder access. It receives one bounded task and has one
total tool-call budget. Depending on its remaining model-step budget and exact
admission, it may be offered `web_search`, a sandbox-only
`request_folder_access` proposal, or the desktop-only
`read_delegated_file`. The delegated read is available only when the foreground
spawn named one exact `{root_id, relative_path}` that was attached to the chat
and remains attached. It accepts no model arguments: the sandbox cannot choose
a root or path, discover neighboring files, or broaden the delegation.

The desktop executor discovers only the parked call identity. Its native-only
claim recovers the server-owned chat, root, and relative path after revalidating
the immutable child admission and current attachment. Immediately before one
bounded UTF-8 broker read, it performs a final authority-checking heartbeat;
the broker independently reauthorizes the chat context and root. Resolution
checks the same authority again, so a detach that wins while bytes are in
flight discards the content and resumes the child with a neutral failure.
Headless servers have no embedded native executor and never advertise this
tool.

The folder-access proposal is a typed terminal child result, not a client call:
it cannot open a
picker, name a root, expose a path, or grant access. Its foreground parent
receives deterministic system context and independently decides whether to
issue the ordinary foreground `request_folder_access` client tool. The worker
atomically parks web search or the delegated read under its exact lease; a
separate host-owned executor performs the bounded operation and writes a
durable receipt. The desktop also persists an app-private delegated-read
receipt before broker dispatch. If a crash makes dispatch ambiguous, recovery
publishes a safe failure instead of reading the file again. On a later claim,
the sandbox reconstructs the matching `ToolUse`/`ToolResult` from the terminal
receipt before it can finalize. Sandboxes do not receive
`spawn_sandbox_agent` or `wait_for_agents` and cannot create further agents.

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

Sensitive server-tool approvals are durable. OpenWave commits the approval
request and its journal event atomically, freezes a renderer-safe approval kind,
and uses exact idempotent decisions. A reclaimed turn resumes persisted pending
calls before requesting another model step, so restart recovery cannot silently
skip an approval or execute a call under a newly relaxed tool policy. Search
consent tells the user that the query and potentially matching document excerpts
may leave OpenWave for the configured AI service.

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
