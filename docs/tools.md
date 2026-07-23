# Tool architecture and roadmap

Tools are the boundary between model decisions and real effects. OpenWave keeps
that boundary small, typed, and explicit: every advertised tool has a stable
name and JSON Schema, an approval class, a bounded result, and a recorded call
identity.

## What exists today

The current foreground agent surface contains fourteen tools:

| Tool | Purpose | Execution boundary |
| --- | --- | --- |
| `read_file` | Read UTF-8 text from private per-chat scratch | Server, read-only |
| `list_dir` | List a private-scratch directory | Server, read-only |
| `write_file` | Atomically write private-scratch text | Server, workspace |
| `exec` | Run a bounded command through the configured execution provider | Server, sensitive native sandbox |
| `search` | Search sources indexed for this exact conversation | Server, read-only |
| `list_sources` | List bounded metadata for sources in this exact conversation | Server, read-only |
| `read_source` | Read a bounded canonical-text range from one source and create citable evidence | Server, read-only |
| `web_search` | Search the public web through the configured Exa or Tavily provider | Server, sensitive approval |
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

Source access is tiered rather than search-only. `list_sources` discovers the
conversation's exact corpus without loading content. `read_source` reads one
bounded Unicode-character range as soon as parser output exists—even while the
embedding job is still running—and returns an opaque reference that produces
the same durable citation cards as `search`. Semantic search remains the
efficient choice for finding passages across a large ready corpus.

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

## Code execution

Command execution lives in `openwave-code-execution`, not in core. Its `exec`
tool accepts one executable, direct argument vector, and private-scratch-relative
working directory. The host—not the model—selects the provider and timeout
through `GET`/`PUT /code-execution`.

The provider contract includes both the canonical tool-call ID and an opaque
chat workspace ID. The former is an idempotency key; the latter lets the local
adapter address private chat scratch and gives future E2B-style adapters a
stable remote-session key. Results normalize exit status, bounded stdout/stderr,
timeout, truncation, provider kind, and duration.

The first provider is local and macOS-native. Seatbelt denies network and
outside-workspace writes, the child inherits no ambient environment or stdin,
and time, output, file size, and open files are bounded. A private durable
running/terminal receipt prevents an ambiguous command from being silently
replayed. Unsupported hosts fail closed with no unconfined fallback. See
[Code execution](code-execution.md).

External MCP servers follow the same rule through `openwave-mcp`. A connected
stdio server completes MCP initialization and paginated `tools/list` discovery
before its proxies are registered. Each remote name is locally namespaced as
`mcp__{server}__{tool}` and keeps the remote input schema unchanged. Calls are
serialized per external server and their text, structured content, and error
flag are translated back into `ToolOutput`. Because MCP tools can cross both the
workspace and process boundary, every mounted proxy is classified `Sensitive`.

The desktop and `openwave serve` boot paths read external stdio servers from the
JSON file named by `OPENWAVE_MCP_CONFIG`. Each entry declares a unique namespace,
an executable plus argument array, an optional working directory and explicit
environment, and an optional bounded request timeout. No shell interprets these
values. Child environments are cleared by default to avoid leaking provider or
host credentials; `env_from` selectively forwards named parent variables without
putting their values in JSON, while `inherit_env` remains an explicit broad
opt-in. A missing selected variable fails startup. Initialization is fail-closed
so the advertised tool surface never silently differs from the boot
configuration. Configuration UI, reconnect supervision, and MCP
`tools/list_changed` refresh remain future work.

## Foreground and sandbox surfaces

Foreground and background agents need different tools. OpenWave should not
advertise every installed integration to every model call.

The foreground coordinator needs tools for:

- conversation-scoped source search plus source list/read;
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

The crate supplies the normalized bounded contract, fixed secret keys, direct
Exa/Tavily adapters through an injected HTTP seam, strict tool-argument
decoding, and the foreground `WebSearchTool`. The server owns a
disabled-by-default provider selection and a 1–60 second request timeout at
`GET`/`PUT /web-search`. That setting has no endpoint or credential reference,
returns only whether the selected fixed key is present, and does not construct
or call a provider.

The foreground registry advertises `web_search` as Sensitive. A call is
persisted, durably approved for sharing the query and explicit filters, and
only then resolves current host policy and credentials. Turn cancellation drops
an in-flight tool future, aborting its HTTP request. The sandbox path remains a
separate checkpoint executor: it attaches host policy only after claiming and
revalidating an exact durable `web_search` checkpoint. Both paths share the
same strict decoder, so unknown fields and out-of-range requests fail before
egress. The concrete transport is bound to the selected provider's exact HTTPS
API domain and rejects scheme, authority, explicit-port, or userinfo deviations
before credentials can leave the process.

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
consent tells the user that the query and potentially matching source excerpts
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
