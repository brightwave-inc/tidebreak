# Tool architecture and roadmap

Tools are the boundary between model decisions and real effects. OpenWave keeps
that boundary small, typed, and explicit: every advertised tool has a stable
name and JSON Schema, an approval class, a bounded result, and a recorded call
identity.

## What exists today

The current agent surface contains five tools:

| Tool | Purpose | Execution boundary |
| --- | --- | --- |
| `read_file` | Read UTF-8 text from private per-chat scratch | Server, read-only |
| `list_dir` | List a private-scratch directory | Server, read-only |
| `write_file` | Atomically write private-scratch text | Server, workspace |
| `search` | Search locally indexed project/chat documents | Server, read-only |
| `request_folder_access` | Ask the trusted desktop host to connect another folder | Client continuation |

The host broker already supports listing connected roots, listing directories,
and reading files, but those operations are not yet advertised directly to the
model. Requesting access and using an approved root remain separate operations.

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

`spawn_sandbox_agent` has a prepared bounded contract and durable foreground
checkpoint path, but is intentionally disabled in the production registry
until a sandbox executor exists. Enabling it earlier would park a foreground
chat with no worker able to complete the child.

A background sandbox starts with a deliberately smaller surface:

- confined command execution;
- web search and image viewing;
- project-document search/read;
- `ask` and non-blocking `notify` communication with its parent;
- a small durable task list;
- explicit `submit_result` completion.

Sandboxes do not receive `spawn_agent`. Ordinary assistant text does not
complete a background run; `submit_result` is its successful terminal action.

Later additions may include a scratchpad, pinned context, plan/execution modes,
deliverable export, clipboard operations, generated images, and connected apps.
Office-suite automation, general computer control, scheduled tasks, enterprise
database tools, and recursive fleets remain deferred.

## Web search

Web search should be a provider-neutral crate rather than HTTP code embedded in
the core loop:

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
