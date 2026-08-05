# Tool architecture and roadmap

Tools are the boundary between model decisions and real effects. OpenWave keeps
that boundary small, typed, and explicit: every advertised tool has a stable
name and JSON Schema, an approval class, a bounded result, and a recorded call
identity.

## What exists today

The current foreground agent surface contains eighteen tools:

| Tool | Purpose | Execution boundary |
| --- | --- | --- |
| `read_file` | Read UTF-8 text from private per-chat scratch | Server, read-only |
| `list_dir` | List a private-scratch directory | Server, read-only |
| `write_file` | Atomically write private-scratch text | Server, workspace |
| `exec` | Run a bounded command through the configured execution provider | Server, sensitive native sandbox |
| `list_sources` | List bounded metadata for sources in this exact conversation | Server, read-only |
| `read_source` | Read a bounded canonical-text range from one source | Server, read-only |
| `read_tool_result` | Read past the point a large tool result was cut short for the turn | Server, read-only |
| `web_search` | Search the public web through the configured provider (Exa, Tavily, Brave, or a self-hosted SearXNG) | Server, sensitive approval |
| `web_extract` | Fetch one exact public page URL, return its readable content through the configured provider or the built-in engine, and keep it as a source of the conversation | Server, sensitive approval |
| `request_folder_access` | Ask the trusted desktop host to connect another folder | Client continuation |
| `list_connected_folders` | List roots already attached to this chat | Native client continuation |
| `list_folder` | List one directory below an attached root | Native client continuation |
| `read_connected_file` | Read bounded UTF-8 text below an attached root | Native client continuation |
| `import_connected_file` | Add one file below an attached root to this chat as a source | Native client continuation |
| `ask_user_questions` | Pause the current turn for one to three bounded user choices | Foreground-only durable user continuation |
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

`import_connected_file` is the one that produces an effect rather than a
reading. It exists because `read_connected_file` returns bounded UTF-8, so a
PDF or Office document under an approved root previously had no route into the
conversation at all — the user had to find the same file again through the
composer picker. The import moves bytes natively into the conversation-scoped
ingest API and returns a document id; it never returns the contents, so the
agent still reads the result through `list_sources` and `read_source` like any
other source. Media type is decided from the bytes rather than from the path
the model named, the attachment is rechecked immediately before the source is
published so a detach that wins the race discards the bytes, and the source
identity is derived from the exact conversation, root, and path, so importing
the same file twice recovers one source instead of adding a second.

### Keeping the renderer's tool vocabulary honest

A renderer-visible tool name used to appear in six hand-maintained places, and
three tools shipped missing an entry in one table or another. The desktop's
union and its runtime guard are now generated from `RendererToolName`, and its
copy and icon tables are keyed on the generated union, so a missing entry is a
compile error. [Wire types](wire-types.md) covers the generator and how to run it.

There is now one copy of the vocabulary: `RendererToolName` in `openwave-core`,
which owns the enum and the single `From<&str>` fold that maps a registered tool
name onto it. The live event projection, the history lookup that rebuilds a
terminal card from the journal, and `ChatToolActivitySnapshot`'s own field type
all use it.

They did not always. The history lookup was a second 20-arm match, and `exec` was
missing from it, so a command read as "Ran a command" while streaming and "Used a
tool" after a reload — with its own command card still visible underneath.

Normal foreground turns also receive a
[host-owned operating prompt](agent-operating-prompt.md). It composes fixed
behavior sections from the exact tool surface advertised to the turn, so source,
folder, output, execution, delegation, and external-tool guidance disappears
when its capability is absent. Tool metadata and runtime host state are never
interpolated into that prompt.

`ask_user_questions` is also foreground-only, but it does not cross the native
executor boundary. The turn atomically checkpoints a renderer-safe question
card and waits. An exact answer completes that same tool call and resumes that
same turn; reload, restart, cancellation, and answer races are storage-backed.
Sandbox agents never receive the definition. See
[Durable user questions](user-questions.md).

`list_sources` discovers the conversation's exact corpus without loading
content. `read_source` reads one bounded Unicode-character range; the text is
available the moment a source is stored, because ingestion decodes
synchronously. The model cites what it reads inline, naming the document id
and a coarse locator — a page or page range, a line range, or a workbook
sheet — and no reference resolution happens server-side.

`web_extract` joins that tier from the other direction: a page it fetches is
stored as an ordinary source of the conversation, so the model can cite the
stored document the same way or put the page URL directly in prose, and
`read_source` can reach the page afterwards. See
[Web search](web-search.md#fetched-pages-as-sources).

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

Provider-backed tools do not belong in core. Web search, connectors, and sandbox
execution supply tools from their owning crates behind the common `Tool`
interface; server-owned source tools use it too.

## Code execution

Command execution lives in `openwave-code-execution`, not in core. Its `exec`
tool accepts one executable, direct argument vector, private-scratch-relative
working directory, and an optional bounded `files` list naming the scratch
paths to stage into a managed sandbox before the command runs — managed
sandboxes see only the listed paths (plus what earlier commands in the same
session created), and only `output/` and `preview/` are copied back afterwards.
The host—not the model—selects the provider and timeout through
`GET`/`PUT /code-execution`.

The provider contract includes both the canonical tool-call ID and an opaque
chat workspace ID. The former is an idempotency key; the latter lets the local
adapter address private chat scratch and gives future E2B-style adapters a
stable remote-session key. Results normalize exit status, bounded stdout/stderr,
timeout, truncation, provider kind, and duration.

The first provider is local and macOS-native. Seatbelt confines writes and
denies direct network access; when the chat grants egress it exposes only one
execution-scoped loopback CONNECT-broker port. The broker admits the chat's
package-registry class, exact hosts, or public internet policy while always
rejecting loopback, private, and link-local targets. The child inherits no
ambient environment or stdin, and time, output, file size, and open files are
bounded. A private durable running/terminal receipt prevents an ambiguous
command from being silently replayed. Unsupported hosts fail closed with no
unconfined fallback. See [Code execution](code-execution.md).

External MCP servers follow the same rule through `openwave-mcp`. A connected
server — a local stdio child or a remote Streamable HTTP endpoint — completes
MCP initialization and paginated `tools/list` discovery before its proxies are
registered. Each remote name is locally namespaced as
`mcp__{server}__{tool}` and keeps the remote input schema unchanged. Calls are
serialized per external server and their text, structured content, and error
flag are translated back into `ToolOutput`. Because MCP tools can cross both the
workspace and process boundary, every mounted proxy is classified `Sensitive`.
Each call needs one explicit approval; MCP approval is never remembered by tool
name because Settings can replace the process behind a stable namespace.

The renderer boundary has one deliberate MCP opening: a tool that declared an
MCP Apps view projects a typed `mcp_app` result reference — the configured
server namespace plus its validated `ui://` URI, never markup — and the
document itself reaches the renderer only through a dedicated view route, to
be rendered inside a sandboxed, non-same-origin frame. Everything else about
an external tool (its remote name, arguments, and output) stays behind the
boundary exactly as before.

The desktop Settings page manages external servers at runtime. Each entry
declares a unique namespace, one transport — an executable with argument array,
optional working directory, explicit non-secret environment, and selected
`env_from` names, or an `http`/`https` URL with a selected bearer-token
variable name — and a bounded request timeout. No shell interprets any field.
Child environments are always cleared before the selected names and literal
values are added, so an MCP process cannot inherit provider credentials or
other desktop secrets by accident. Resolved `env_from` and bearer-token values
never enter the database or renderer, and child stderr is discarded instead of
being copied into host logs.

Saving first validates and connects the complete candidate set, then atomically
publishes it for later turns. A failure leaves the prior connection set active.
Active turns retain their immutable registry snapshot, so reconnect or tool
refresh cannot change a replaying turn's schema or executor. A supervisor probes
idle sessions, skips sessions busy with a live call, reconnects with capped
backoff, and establishes a fresh connection after
`notifications/tools/list_changed` before publishing the new tool list.
Provider-safe names plus per-frame, per-tool, count, pagination, and aggregate
metadata limits keep a malformed server from breaking or bloating later model
requests.
The legacy `OPENWAVE_MCP_CONFIG` JSON file remains available for headless boot;
when no saved configuration exists it uses the same closed schema and fails
startup if an enabled server cannot initialize.
See [External MCP servers](mcp-servers.md) for the field contract and setup flow.

## Foreground and sandbox surfaces

Foreground and background agents need different tools. OpenWave should not
advertise every installed integration to every model call.

The foreground coordinator needs tools for:

- conversation-scoped source list/read;
- conversation-private deliverable creation for explicit native export;
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
network, or broad host-folder access. It receives one bounded task and works
through a bounded budget of tool-call checkpoints. Depending on its remaining
model-step budget and exact admission, it may be offered `exec`, `web_search`,
a sandbox-only `request_folder_access` proposal, or the desktop-only
`read_delegated_file`.

`exec` runs against a workspace named by the run itself, so concurrently
delegated siblings never share scratch. It carries no folder authority and
stages no host paths: delegation already runs outside the conversation's
approval gate, and the sandbox filesystem is the whole of what a delegated run
can reach. Files the run leaves under `output/` are published to the parent
conversation as outputs named by their own filenames — writing the same filename
again produces the next version of that output rather than a second one. The run
never names an output identity, and neither does the host.

The delegated read is available only when the foreground
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
clipboard operations, generated images, richer deliverables, and connected
apps. Office-suite automation, general computer control, scheduled tasks,
enterprise database tools, and recursive fleets remain deferred.

## Web search

Web search now has a provider-neutral `openwave-web-search` crate rather than
HTTP code embedded in the core loop:

```text
WebSearchProvider
├── ExaProvider
├── TavilyProvider
├── BraveProvider
└── SearxngProvider

WebSearchService
├── timeout and retry policy
├── provider selection and health
├── normalized result ranking
└── URL deduplication

WebSearchTool
├── model-facing schema
├── credential-free request
├── bounded normalized output
└── optional durable web-document ingestion
```

The crate supplies the normalized bounded contract, fixed secret keys, direct
vendor adapters over plain HTTP through an injected seam, strict tool-argument
decoding, and the foreground `WebSearchTool`. The server owns a
disabled-by-default provider selection and a 1–60 second request timeout at
`GET`/`PUT /web-search`. That setting carries no credential reference and does
not construct or call a provider. The one address it accepts is the base URL of
a self-hosted SearXNG instance, which is validated at `PUT` time and is the only
provider address that is not a constant; see [Web search](web-search.md) for why
that stays safe.

The foreground registry advertises `web_search` as Sensitive. A call is
persisted, durably approved for sharing the query and explicit filters, and
only then resolves current host policy and credentials. `web_extract` sits
beside it on the same terms: Sensitive, with the exact page URL on the
approval card, and routed deterministically to the configured provider when it
implements the extract contract or to the built-in native extraction engine
otherwise. Each page it returns is also kept as a conversation source the model can cite. Exa and Tavily both implement it, so a configured host extracts
through the vendor and falls back to native — except on a rejected key, which
surfaces for repair rather than degrading silently. Brave and SearXNG are
search-only, so a host on either extracts natively. See
[Web search](web-search.md) for the routing, wire shapes, and fetch-policy
details. Turn cancellation drops
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
skip an approval or execute a call under a newly relaxed tool policy. Web
search consent tells the user that the query may leave OpenWave for the
configured search provider.

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
