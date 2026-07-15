# How OpenWave works

This is a maintainer's tour of OpenWave: what the product is today, what happens
when someone uses it, why the code has several asynchronous state machines, and
where the unfinished edges are. It is meant to be readable before you know the
Rust code or the history behind it.

OpenWave is pre-alpha. The runtime is substantially more complete than the
desktop experience, and both the schema and crate boundaries are still free to
change.

## OpenWave in one minute

OpenWave is a local agent runtime. A user chooses a model, starts a chat, and can
connect one or more folders that the project or conversation is allowed to use.
The runtime can ask the model for an answer, call tools over those connected
folders, search an indexed document corpus, pause for approval before a
sensitive action, and stream progress back to the client.

The important architectural choice is that the user-facing request and the
potentially long-running work are separate:

1. The API first records exactly what was requested.
2. A background worker claims that work with a temporary lease.
3. The worker performs model, tool, parsing, or indexing work.
4. The final result is committed only if that exact worker still owns the work.
5. A retry of the same command can discover the already-committed result instead
   of doing the work twice.

That is the reason for the job records, statuses, revisions, lease tokens, and
event sequence numbers. They are not product concepts a user should have to
manage. They keep durable state transitions atomic and idempotent, and make jobs
restartable where their side effects are safe to replay. Arbitrary tool effects
are not exactly-once yet.

## The system at a glance

```text
                 Desktop UI or local client
                            |
                    HTTP + WebSocket
                            |
                 +----------v----------+
                 |  openwave-server    |
                 | routes + workers    |
                 +---+------+-------+--+
                     |      |       |
          model calls|      |       |file/search tools
                     |      |       |
              +------v--+   |   +---v------------------+
              | router  |   |   | host broker + search |
              +---------+   |   +----------------------+
                            |
        +-------------------+--------------------+
        |                   |                    |
  operational store    source blob store    Lance vector index
  chats, turns, jobs    immutable raw bytes  derived search data
  SQLite today          local filesystem     local filesystem
```

There are three different kinds of persisted data because they have different
jobs:

- The **operational store** is the source of truth. It contains chats, messages,
  turns, event history, document metadata, canonical text, and worker state.
- The **blob store** retains immutable original document bytes. It makes a later
  reparse possible and deduplicates identical content.
- The **Lance index** is a derived search structure. It contains chunks and
  embeddings and should always be rebuildable from authoritative data.

Keeping the index separate is deliberate. Losing or rebuilding a search index
must not mean losing the source document.

## Startup, configuration, and local data

Both the desktop and `openwave serve` use the same server boot path. The native
desktop additionally enforces one application instance, owns folder consent,
and lazily starts its bundled host-broker sidecar. Server startup acquires
exclusive ownership of the data directory, opens the operational database and
Lance index, loads provider configuration, registers tools, starts the turn and
document workers, and finally binds an ephemeral loopback port with a fresh
bearer token.

The main local data is easy to recognize:

| Location | Meaning |
| --- | --- |
| `openwave.db` | SQLite operational source of truth |
| `blobs/` | retained immutable document bytes |
| `vectors/` | derived Lance search index |
| `openwave.lock` | proof that one process owns this data directory |
| OS keychain | provider credentials, kept outside the database |

Non-secret provider settings and the default model live in the operational
store and can change while the process runs. Model routing observes those
changes without restart. The embedding backend is different: it is selected at
startup because the Lance index has a fixed vector width, so enabling OpenAI
embeddings takes effect after a restart.

The model catalog shows built-in choices even before a key is configured. This
helps configuration, but it does not bypass egress safety: a turn still fails
closed until a matching provider is enabled and credentialed. OpenAI-compatible
routes currently require a nonempty credential even when a local endpoint would
otherwise accept unauthenticated requests.

## What happens during a chat turn

### 1. The server accepts a turn

The client creates a stable `turn_id` and posts the user's text to
`POST /chats/{chat_id}/messages`. The server resolves the model in this order:

1. the chat's model override;
2. the mutable global default;
3. the boot default.

The operational store commits the user message and a queued `TurnRun` together.
Only then does the API return `202 Accepted`. Repeating the same `turn_id`, chat,
and text returns the existing accepted turn. Reusing the identity for different
input is a conflict.

Each chat has one live durable turn slot. This makes ordering and transcript
construction straightforward. A second turn is not queued behind the first; the
server rejects it with `409 Conflict`, and the client can retry after the live
turn finishes.

### 2. A worker claims it

The turn worker scans for due work and changes the turn from `queued` to
`running`. The claim installs a random lease token and expiry. Think of the lease
as a temporary claim ticket:

- heartbeats keep the ticket valid while work is progressing;
- completion, failure, and cancellation must present the same ticket;
- a stale worker cannot commit completion or journal events after losing
  ownership;
- an expired claim can be resolved conservatively.

The turn worker can process up to four different chats concurrently. A turn is
currently configured for one execution attempt because arbitrary tool side
effects are not yet checkpointed well enough to replay safely after a crash.

### 3. The agent drives model and tool steps

The agent rebuilds a provider transcript from persisted messages and structured
tool-call records. It then loops for a bounded number of model steps:

1. fit the transcript into the model's context window;
2. call the selected provider and stream its output;
3. durably accept each tool call's canonical name and arguments, then run it;
4. feed tool results back to the model;
5. repeat until the model produces a final answer.

The built-in server registry currently contains:

- `read_file` and `list_dir`, which are read-only;
- `write_file`, which is workspace-confined and runs without a prompt;
- `search`, which queries the indexed corpus and requires approval only when its
  embedder, store, or reranker can send data outside the machine.

Today these file tools still operate inside one directly opened workspace
directory stored on the chat. That temporary tool path is not the intended
product boundary. The desktop can now connect, list, and revoke multiple folders
through a native picker and capability-gated host-broker sidecar; it exposes only
opaque root IDs and display names to the renderer. The next tool-routing slice
will resolve file operations through those roots instead of the legacy workspace.
See [Host access and connected folders](host-access.md). An agent may later ask
the desktop to connect another folder, but the request only opens a native consent
flow; it never grants a named path by itself. The model router supports Anthropic, OpenAI, and
OpenAI-compatible endpoints. It fails closed: if no enabled provider with a
usable credential can serve the selected model, no model request is sent.

### Tool calls are small state machines too

A tool call is not a mutable log row. Its `CallId`, provider identity, name,
arguments, owning turn, and execution surface are accepted once and then remain
immutable. Repeating the same acceptance recovers the existing call; reusing the
identity with different arguments is a conflict. A server tool then makes one
terminal transition to `completed`, `failed`, or `cancelled`, and an exact retry
recovers that result rather than overwriting it.

The same record can describe work that must execute on a trusted client, such as
a future native folder-picker request. Client work starts unclaimed and is
listed from durable storage for reconnect recovery. A client claims it with an
executor identity and a fresh secret claim token; the store installs that token
with the expiry. Heartbeat and completion require it, so an old callback from
the same desktop process cannot act as a newer claim. The token is returned only
by the claim operation; ordinary pending/history records and their serialized
forms never contain it. Server code cannot complete client work, and a client
cannot claim ordinary server tools. An expired native interaction
is deliberately not handed to another client automatically: the first picker or
broker mutation may have happened even if its receipt was lost. A separate
exact-token recovery transition can record the broker's known result or an
authoritative abandonment without replaying the native action.

```text
                         +----success----> completed
pending ----execute----->+----failure----> failed
                         +----decline----> cancelled

client execution adds: unclaimed ----claim/heartbeat----> leased
```

The local API exposes this durable boundary through authenticated per-chat
pending, claim, heartbeat, and resolve routes. Lease times are server-owned. The
client supplies the stable executor identity and fresh secret token, so retrying
a claim after a lost HTTP response recovers the original claim without trusting
the client clock. Resolution retries converge on the token and terminal payload;
the first server timestamp remains metadata rather than request identity. The
resolve route can also reconcile a known result after expiry under the exact
token, but it never transfers ambiguous native work to a second executor.
Heartbeats are simpler monotonic liveness updates: each accepted repeat renews
the lease from the server's current receive time. Heartbeat and resolution
enforce the immutable chat owner inside the same locked state transition rather
than loading conversation history first.

The store can now hand a turn across that process boundary without keeping its
worker alive. In one transaction it records the immutable client call, records a
wait receipt tied to the exact worker claim, moves the turn to
`waiting_for_client`, folds that lease segment's model-step and token-usage delta
into the turn-wide totals, and releases the worker lease. The immutable receipt
keeps the same progress delta. If the database commit is ambiguous, the worker
can repeat the exact request and recover that receipt without charging the delta
twice; changing the call or its accounting is an identity conflict.

Resolving the client call closes the receipt and moves the turn to `resuming` in
the same transaction. A turn worker then takes a fresh lease segment without
spending another failure attempt. It subtracts the checkpointed model calls from
the turn-wide step budget and seeds its aggregate usage from the durable total.
The persisted totals also let cancellation report work performed before the
pause, including when cancellation wins before the resumed agent starts. This is
the durable equivalent of pausing a function, doing native work elsewhere, and
continuing from a known checkpoint.
The remaining integration work is to have the agent emit this checkpoint for
folder-access tools and to run those pending calls from the desktop. Notifications
will be wake-up hints; the pending-work query remains authoritative after a
restart or missed notification.

### 4. Events drive the live client

Text deltas, tool activity, approvals, steering, cancellation, and the terminal
result are `AgentEvent`s. Before an event is broadcast live, the worker normally
appends it to a per-chat journal with an increasing sequence number.

A reconnecting WebSocket client sends the last sequence number it saw. The
server first subscribes to the live tail, replays newer journal entries, then
delivers buffered and new live events while discarding overlap. The journal is
the durable receipt tape; the broadcast channel is only the fast delivery path.

### 5. The result becomes terminal

Successful completion commits the final assistant message, the completed turn
state, and its terminal event as one operational transaction. Failure and
cancellation have equivalent exact-resolution paths. This prevents a UI from
seeing “completed” while the answer itself is missing.

Completed, failed, and cancelled are **terminal** states: no later worker may
change that turn without a new explicit workflow.

The turn state machine is:

```text
queued ----claim----> running ----success----> completed
  |                     |
  |                     +----failure---------> failed
  |                     |
  |                     +----cancel request--> cancelling --> cancelled
  |                     |
  |                     +----park native call----> waiting_for_client
  |                                                   |       |
  |                                            resolve|       |cancel unclaimed
  |                                                   v       v
  |                                               resuming  cancelled
  |                                                   |
  |                                         fresh claim segment
  |                                                   |
  |                                                   +-----> running
  |
  +----cancel-----------------------------------------------> cancelled

waiting_for_client ----cancel claimed call----> cancelling_client
                                                    |
                                         exact client resolution
                                                    |
                                                    +-----> cancelled
```

retry_wait exists for safely replayable failures. `attempt_count` tracks that
failure/replay budget, while `claim_count` advances for every worker lease. A
resume therefore gets a fresh exact lease without pretending the client round
trip was a failure or consuming another attempt. Ordinary turns still default
to one failure attempt because external and filesystem effects are not yet
fully checkpointed.

## Cancellation, approvals, and steering

These are all ways for the human to interact with work that is already running,
but they have different durability today.

### Cancellation

Cancellation is a durable state transition. A queued turn can become cancelled
immediately. A running turn first becomes `cancelling`; its exact worker receives
a local cancellation signal, stops cooperatively, and acknowledges `cancelled`.
An unclaimed client call can also be cancelled immediately: the call, wait
receipt, turn, and terminal event change together. If the native client already
claimed the call, the turn becomes `cancelling_client` until that exact client
lease reports a terminal result. This avoids pretending an in-flight folder
picker or broker mutation never happened. In every case the chat remains
occupied until the owner has quiesced or the store has atomically fenced the
work, so a new turn cannot race the old one.

### Approvals

A sensitive tool call emits `ApprovalRequired` and parks until the user approves
or rejects it. Approval waiting is process-local and the server enforces one
process per data directory, so an application restart does not yet resume a
parked approval.

### Steering

Steering lets the user add an instruction to the current turn. The request has
its own stable `steer_id` and is accepted durably as `pending`. The claimed worker
applies pending instructions in order, commits each as a user message, advances a
steering revision on the turn, and either continues at the next model boundary or
interrupts the current provider stream.

Completion requires both that no accepted steer remains pending and that the
steering revision still matches the one captured before the model request. The
pending check catches instructions not yet applied; the revision check catches
an instruction applied while the model was generating. In either case, the
worker regenerates the answer with the new instruction instead of committing a
stale response.

The steering state machine is small:

```text
pending ----worker applies----> applied
   |
   +----turn fails/cancels-----> rejected
```

Applying a steer commits its user message, application receipt, revision, and
`UserSteered` replay event in one transaction. If the response to that commit is
lost, the worker retries the same identity and recovers the exact existing
result rather than applying the instruction twice.

## What happens when a document is added

A project does not automatically crawl its workspace. Documents enter the
system through an explicit HTTP upload. A source URI is identity and provenance;
OpenWave does not fetch that URI itself.

### 1. Publish immutable source bytes

The server hashes the upload and writes it to the blob store under a
content-derived identifier. Identical bytes can therefore be shared safely by
more than one document. Blob publication happens before the catalog transaction,
so an accepted catalog record never points at bytes that were not written.

### 2. Accept a new document generation

The operational store creates or updates the stable document record and queues a
`Parse` job in the same transaction. The API returns `202 Accepted`; parsing and
embedding happen later.

This is where “revision” and “generation” matter:

- `DocumentId` means “this logical document.” A URI gives it a stable identity;
  project scope is included so the same URI in different projects stays separate.
- `content_revision` is a counter: first content is 1, replacement is 2, and so
  on. Deletion also advances the clock.
- `revision_token` is a random identity for that exact revision.
- Together, the revision and token are a `DocumentGeneration`.

The retained integer clock says which version is newer and keeps increasing
through deletion and recreation, preventing ordinary version reuse. The token
adds exact identity, so even an unexpected equal revision cannot be mistaken for
the generation a worker originally claimed.

This model is more explicit than a simple mutable document row because parsing
and embedding happen outside the database and may take minutes. Every worker
completion needs to prove, “I processed exactly the generation that is still
current.”

### 3. Parse into canonical text

A document worker claims the `Parse` job with a lease, verifies the retained
byte length and SHA-256 digest, and parses it into:

- canonical UTF-8 text, the text-of-record used by indexing;
- source regions that map text byte ranges back to locations such as pages.

The parse completion and creation of the matching `Index` job are atomic. Today
the configured parser registry handles `text/*` content. Richer PDF and office
parsers are future work. The model can represent page provenance, but the
production plain-text parser does not emit page regions today.

### 4. Chunk, embed, and stage

The index worker splits canonical text into overlapping structural chunks,
creates embeddings, and writes the exact generation to a staged area in Lance.
Search still sees the prior active generation during this work.

The default is an offline deterministic hash embedder. If OpenAI is enabled and
has a credential at startup, OpenWave uses `text-embedding-3-small` instead. The
choice is intentionally gated by the provider's enabled flag so documents are
not sent to an external embedding service unexpectedly. With OpenAI embeddings,
both document chunks and later search queries leave the machine.

### 5. Activate, then mark ready

After staging, the worker rechecks its exact live lease, activates the generation
in Lance, and records the indexed revision in the operational store. The document
then becomes `ready`. A stale worker is fenced at every publication boundary and
cannot overwrite a newer upload.

There is no document event stream yet. After receiving `202`, a client polls the
document list or detail endpoint for `queued → processing → ready/failed`. The
public record exposes the failed state but not the underlying job's error detail;
operators currently rely on logs, and the retry endpoint revives only the current
exact failed job when it is still compatible with the active pipeline.

The durable job state machine is:

```text
queued --> running --> succeeded
            |   |
            |   +--> retry_wait --> running
            |
            +------> failed

superseded work and deleted documents become cancelled instead of publishing.
```

An auditor periodically compares authoritative documents, active parser/index
fingerprints, jobs, and derived index coverage. It can repair missing work or
schedule reprocessing after a pipeline change. Delete uses a durable tombstone so
an old vector generation cannot reappear after removal.

### 6. Retire unused blobs

Replacing or deleting a document may make its old source blob unreferenced. A
separate leased retirement worker rechecks all authoritative references before
deleting it. A grace-period orphan auditor also reclaims blobs that were written
but never became referenced because a later catalog transaction failed.

## How search works

Search is scoped either to explicitly unscoped documents or to one project. The
same retriever backs the HTTP search endpoint and the agent's `search` tool.

For each query, OpenWave:

1. embeds the query;
2. retrieves both dense-vector matches and lexical BM25 matches;
3. combines their ranks with reciprocal-rank fusion;
4. can rerank a larger candidate set when a reranker is configured;
5. suppresses heavily overlapping passages and adds modest document diversity;
6. returns bounded citations with source spans and provenance.

In plain language, lexical search matches words, vector search matches embedding
proximity, and rank fusion combines the two result lists. The vector leg becomes
meaningfully semantic when OpenAI embeddings are active; the offline hash
embedder is a deterministic local baseline. The reranker seam exists, but the
server does not configure a reranker today.

The current Lance implementation is durable and generation-aware, but search is
still a flat scan rather than an approximate nearest-neighbor (ANN) index. That
is an intentional pre-scale baseline, not the final large-corpus performance
design.

## The different ways to run OpenWave

### Desktop

The Tauri host starts the same server in-process on an ephemeral loopback port,
mints a fresh bearer token, and gives its React webview the address and token.
The browser-facing API is not exposed on a public network interface.

The current UI is a walking skeleton, not the complete product. It creates a new
loose chat on each launch and supports provider setup, model selection, basic
chat streaming, approval prompts, and native connected-folder pick/list/revoke.
OpenWave's operational scratch stays in private app storage; user-selected paths
cross only the native host-to-broker control boundary, while the renderer sees
opaque folder summaries. Built-in agent file tools still use the temporary
private scratch path and are not yet routed through those connected roots. The
UI does not yet provide a chat picker, history reconstruction, projects,
document ingestion, document search, cancel, or steer controls, even though much
of that backend API already exists.

Because that chat is projectless, its `search` tool can see only the unscoped
document corpus. Project-scoped documents are not reachable through the current
desktop journey.

There is also no public message-history endpoint yet. Initial user messages are
persisted but are not journal events, so the event stream alone cannot rebuild a
complete old conversation. A real reopen/history experience needs an explicit
snapshot API in addition to journal replay.

### Headless server

`openwave serve` starts the same local HTTP/WebSocket server and prints the
loopback URL and launch token for a parent process or local script. The desktop
and headless paths therefore exercise the same route and worker code.

There is no generated OpenAPI or standalone API reference yet. The route table,
request/response types, and handler documentation in `openwave-server` are the
effective API specification.

### MCP

`openwave mcp /absolute/workspace` starts an MCP stdio server confined to that
workspace. It currently exposes only `read_file` and `list_dir`, and only after a
proper MCP initialize lifecycle. Indexed search, approval-aware writable tools,
and the MCP client that mounts external servers into OpenWave are not wired yet.

### Self-host

The `Profile::SelfHost` shape and PostgreSQL database implementation exist, and
CI exercises the durable turn state machine against PostgreSQL. The server still
rejects the self-host profile at startup, and document/blob PostgreSQL parity is
not comprehensively tested. Production Postgres wiring, remote secret custody,
object storage, and multi-process ownership remain future integration work.

## Where the code lives

| Area | Start here | What it owns |
| --- | --- | --- |
| Core contracts and model | `crates/openwave-core/src/lib.rs`, `model.rs`, `storage.rs` | Typed IDs, persisted records, store/tool/provider traits |
| Agent loop | `crates/openwave-core/src/agent.rs` | Model/tool loop, context fitting, cancellation, steering |
| Operational database | `crates/openwave-core/src/db/` | Schema plus transactional SQLite/Postgres state transitions |
| Model providers | `crates/openwave-router/src/` | Anthropic/OpenAI adapters and model-to-provider routing |
| Local API | `crates/openwave-server/src/lib.rs`, `routes.rs`, `routes/client_execution.rs` | Authentication, API assembly, chat, turn, and leased client-execution routes |
| Turn execution | `crates/openwave-server/src/turn_worker.rs` | Claiming, heartbeats, event journaling, terminal resolution |
| Documents | `crates/openwave-server/src/routes/document.rs`, `document_worker.rs` | Upload API and Parse/Index worker orchestration |
| Retrieval | `crates/openwave-retrieval/src/` | Parsing, chunks, embeddings, Lance, ranking, citations |
| Desktop | `crates/openwave-desktop/src/`, `crates/openwave-desktop/ui/src/App.tsx` | Tauri host and current React shell |
| Host access (planned) | `docs/host-access.md` | Broker trust boundary, connected-root model, and delivery slices |
| MCP | `crates/openwave-mcp/src/`, `crates/openwave-cli/src/main.rs` | MCP protocol server and stdio command |
| Connectors and Slack | `crates/openwave-connectors`, `crates/openwave-slack` | Placeholders, not working product surfaces yet |

The dependency direction is intentionally simple: clients compose libraries,
and libraries point down toward `openwave-core`. Core defines the contracts; it
does not depend on HTTP, Tauri, or a particular model vendor.

## The reliability rules to preserve

When changing the runtime, these rules matter more than the exact module layout:

1. **Acknowledge only durable acceptance.** A `202` means the command and its
   identity committed, not merely that an in-memory task was spawned.
2. **Give commands stable identities.** Ambiguous network retries should recover
   the first result; the same identity with different input should conflict.
3. **Make related facts atomic.** Input plus queued turn, parse completion plus
   index enqueue, and final answer plus completion event belong in transactions.
4. **Fence expensive work.** Workers must present an exact lease and exact source
   generation before publishing.
5. **Treat notifications as hints.** A `Notify` or process-local signal may reduce
   latency, but durable polling must eventually find all accepted work.
6. **Keep the index derived.** Canonical source and provenance remain in the
   operational store; Lance can be repaired or rebuilt.
7. **Retry exact requests after ambiguous failures.** Do not invent new IDs or
   timestamps until it is known that the original operation did not commit.
8. **Do not replay unknown side effects.** Until tool execution has durable
   checkpoints and idempotency contracts, fail conservatively after ambiguity.
9. **Journal before live publication.** Reconnect correctness comes from the
   journal, not the in-memory broadcast bus.
10. **Test state machines against both databases.** SQLite is the desktop path;
    PostgreSQL tests catch different transaction and locking behavior.

## What is solid, and what comes next

The strongest parts today are the core seams, database constraints, durable
document pipeline, generation-aware Lance publication, durable turn acceptance
and ownership, atomic client-wait checkpoints, terminal turn commits,
cancellation, reconnectable event stream, and fail-closed provider routing.

The main next steps are:

- replace raw chat/project workspace paths with broker-mediated, per-context
  connected roots while keeping OpenWave's private app data separate;
- teach the agent to emit client-wait checkpoints, notify clients of durable
  pending work, and run native folder requests in the desktop;
- add resumable checkpoints and explicit idempotency policy around remaining
  server-side tool effects;
- make approvals resumable rather than process-local;
- expose existing backend capabilities through a real desktop information
  architecture: chat history, projects, documents, search, cancel, and steer;
- add richer parsers and wire indexed search into MCP;
- build the MCP client and connector surfaces;
- finish the self-host profile rather than only testing Postgres state logic;
- add health-aware provider failover;
- bound the agent-to-worker event channel and batch or page journal traffic so
  long, fast turns cannot create unbounded memory or replay work;
- introduce ANN and maintenance policy when corpus measurements justify it;
- add frontend typecheck/build coverage, platform CI, readiness/metrics, graceful
  shutdown, and backup/repair documentation;
- split remaining very large implementation and test modules along stable domain
  boundaries as those boundaries settle;
- condense the pre-v1 migration history once the model stabilizes.

## Working in the repository

The repository declares the stable Rust toolchain and pins dependencies in its
lockfile. Public contributor commands are deliberately ordinary Cargo commands:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked
```

CI runs formatting, Clippy, build, headless tests, desktop tests, and the
PostgreSQL turn-state suite in parallel. Compiler outputs use `sccache`; Cargo
downloads share a trusted-main cache. Documentation-only changes skip the Rust
lanes but still run the secret scan.

For a runtime change, prefer a small vertical slice with state-transition tests,
SQLite coverage, and PostgreSQL coverage when database locking or transactional
semantics are involved. Keep public routes thin: orchestration belongs in the
server, while reusable state transitions belong in `openwave-core`.

Before v1, there are no deployed users to migrate. Schema work may update and
reset the existing migration set instead of preserving obsolete compatibility
steps; the migrations should be condensed into a clean baseline before v1.

## Glossary

- **Chat:** a conversation container that resolves an explicit host-access
  context; it does not own an arbitrary absolute workspace path in the intended
  model.
- **Connected root:** a user-approved host folder known to tools by an opaque
  root identifier and root-relative paths.
- **Turn:** one accepted unit of user-to-agent work inside a chat.
- **Message:** durable user or assistant text; tool calls are stored separately.
- **Tool call:** an immutable canonical request plus a pending or terminal
  execution state; client-owned calls also carry an exact temporary lease.
- **Event journal:** ordered receipts used to rebuild a live client stream.
- **Lease:** temporary, renewable worker ownership of exact work.
- **Revision:** the monotonically increasing version number of a document or the
  steering freshness counter of a turn.
- **Revision token:** random exact identity paired with a document revision.
- **Generation:** the revision number and revision token together.
- **Fingerprint:** stable identity of parser, chunker, or embedder behavior.
- **Canonical text:** authoritative parsed text from which the search index can
  be rebuilt.
- **Watermark:** the document revision known to be active in the derived index.
- **Tombstone:** an explicit empty/deleted generation that prevents old derived
  data from reappearing.
- **Fence:** a check that rejects completion from stale ownership or stale input.
- **Idempotent:** safe to repeat with the same identity and receive the same
  logical result.
