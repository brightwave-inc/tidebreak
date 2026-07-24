# Host access and connected folders

OpenWave is local-first, but "local" should not mean that every agent can see
every file on the machine. This document describes the intended boundary between
OpenWave's product state and user-approved access to the host computer.

The design follows the same basic product split as Brightwave web plus desktop.
The important difference at first is deployment: OpenWave's control plane runs
locally. The boundary is still explicit so that a self-hosted or managed control
plane can use the same contracts later.

## The product model

OpenWave owns an application data directory. It contains the operational
database, retained source blobs, the search index, broker state, audit records,
and private scratch data. It is not a user project and should not appear as the
one folder all conversations work in.

A folder elsewhere on the machine is a **connected root**. It becomes available
only after the user chooses it through a trusted desktop action, such as a native
folder picker. The broker remembers that host approval so the user does not have
to locate the same folder again, but usable authority is attached through the
exact conversation. Each conversation can contain more than one connected root.

In practical terms:

- OpenWave may always use its own private application data and scratch space.
- Each conversation has its own ordered set of connected folders.
- A new conversation does not inherit another conversation's roots or fall back
  to a shared workspace.
- Previously approved roots may be offered by safe display name in another
  conversation. Attaching one requires a trusted native confirmation, but not
  another folder picker.
- If multiple approved roots have the same safe display name, none of those
  ambiguous roots are offered for reuse; the user must identify one through
  the native picker.
- The model sees an opaque root identifier, a display name, and root-relative
  paths. It never receives authority by naming an absolute host path.
- Connecting one folder never grants access to its siblings, the user's whole
  home directory, or another conversation's roots.
- An agent can request access to another folder, including familiar locations
  such as Documents or Downloads. The request can explain why and suggest where
  to open the picker, but only the folder the user actually selects is granted.

Chats persist only ordered opaque root IDs plus an attachment revision. These
product rows are not grants: they contain no path, capability, consent, or
display name, and host access still requires the broker's live attachment and
authorization. The product store has a durable attachment-change state machine
and a native-only HTTP boundary for driving it. Both user-facing connected-folder
actions and agent-approved picker requests now converge the broker attachment
and product projection through that state machine before reporting success.

Project rows and lower-level project APIs remain in the runtime but are dormant
in the current desktop product. Those APIs can still create a project-scoped
chat and snapshot its project's opaque root projection into the new chat. This
is compatibility behavior, not a surfaced workflow or a promise of automatic
Project inheritance. If Projects return, they should be a completely optional
layer above otherwise self-contained conversations.

### Native attachment-change boundary

The local control plane exposes three attachment-change routes to trusted native
code:

- `POST /chats/{chat_id}/root-attachment-changes/{change_id}/begin` records an
  exact attach or detach intent against the caller's observed projection
  revision.
- `GET /root-attachment-changes/pending?limit=64` recovers awaiting work for
  this desktop's stable private executor identity, oldest first.
- `POST /root-attachment-changes/{change_id}/finish` records the broker's exact
  terminal observation and commits the corresponding projection transition.

These routes require both the ordinary bearer credential and the native
client-executor credential. The server supplies the stable executor identity;
it is never accepted from a body, returned in a response, included in server
discovery, or exposed to the renderer. Begin and finish retries return stable
`begun`/`existing` and `finished`/`existing` dispositions. A missing change and
a change owned by another executor are both reported as not found, while busy
and revision conflicts do not identify the pending operation.

Conflict responses carry stable machine-readable `kind` values so the native
reconciler never branches on prose: `root_attachment_identity_conflict`,
`root_attachment_revision_conflict`, `root_attachment_capacity_exceeded`,
`root_attachment_revision_exhausted`, `root_attachment_chat_busy`,
`root_attachment_already_terminal`, or
`root_attachment_broker_state_mismatch`. The generic server embedding does not
mount these routes because it has no restart-stable native executor identity;
the desktop supplies its private persisted identity before binding the server.
Finish timestamps are clamped to immutable creation time under the store lock,
so clock skew cannot strand a conversation's single pending operation.

This boundary deliberately does not call the broker. The desktop reconciler
looks up the exact broker receipt first, dispatches the same idempotent attach
or detach identity only when it is unknown, and then supplies the terminal
observation to the product state machine. Startup recovery is sequential and
bounded to 64 oldest pending changes per pass.

### Exact files delegated to a background agent

Foreground folder tools can browse roots already attached to their chat. A
background sandbox receives a much narrower capability: when spawning the
child, the foreground agent may name one already attached opaque root ID and
one root-relative file path. That exact pair is stored immutably in the child's
admission. It does not attach a root, create a broker grant, or expose a host
path.

Only the embedded desktop advertises `read_delegated_file`, and only to the
depth-one child whose admission has that exact resource while the root remains
attached. The tool takes an empty argument object, so the child cannot replace
the target or discover other roots and files. It shares the sandbox's one total
tool-call budget with web search and the typed folder-access proposal. It is not
a picker, directory listing, write, shell, or general filesystem capability.

The read crosses the trust boundaries as a durable continuation:

1. The sandbox parks one argument-free call and releases its worker lease.
2. The desktop polls a native-only pending route that reveals only call IDs.
   Pending, claim, heartbeat, and resolve require both the ordinary loopback
   bearer and a separate native-executor credential withheld from the renderer.
3. Claim installs an exact executor lease and revalidates the immutable child
   admission plus the chat's current attachment before returning the opaque
   root and relative path to native code.
4. Native code durably records a private dispatch fence, then sends a final
   specialized heartbeat. That heartbeat revalidates the admission and
   attachment immediately before broker admission.
5. The host broker performs one bounded UTF-8 `ReadFile` under the
   server-derived conversation context. It checks its own live attachment and
   grant and reauthorizes before releasing bytes.
6. Resolve revalidates the executor lease and current product attachment before
   committing bounded content. If a detach or cancellation won the race, the
   content is discarded and the sandbox resumes with a neutral terminal result.

The app-private recovery receipt contains the call identity, stable executor,
secret lease, dispatch phase, and bounded terminal resolution, but not the root
or relative path. A receipt recorded after dispatch began is never dispatched
again after a crash: if no terminal result was durably stored, recovery resolves
the call as unavailable. This conservative no-replay rule treats even a
privacy-sensitive read as an effect whose ambiguous dispatch must not be
repeated. An expired claim whose private receipt was lost is likewise
cleanup-only, never authority for another read. A target that cannot be
represented by the broker's stricter relative-path contract fails neutrally
before host I/O.

The headless server does not have the embedded native executor or its stable
private credential, so it never advertises this tool. Future self-hosted or
managed deployments will need an equivalent explicitly trusted host executor;
the current implementation does not claim that support.

## The four layers

```text
             web UI / desktop renderer
                 product actions
                        |
                        v
          OpenWave control plane (local first)
       projects, chats, turns, documents, tools
                        |
             capability-checked operations
                        v
             host-broker sidecar process
       grants, path policy, host I/O, audit log
                        ^
                        |
             trusted consent controls only
                        |
                 Tauri desktop host
          native picker and permission dialogs
```

The **control plane** owns product meaning. Today it is the loopback
`openwave-server` embedded in the desktop app or started by `openwave serve`.
Later it may be self-hosted or managed. Agent turns can request host operations,
but the control plane cannot turn an agent request into a new grant.

The **desktop host** owns native, human-originated consent. It can open a folder
picker and send the selected path to the broker's trusted control channel. It
does not implement path policy or filesystem tools itself.

The **host broker** is the machine trust boundary. Its transport-neutral core
owns the root registry, capability grants, path confinement, filesystem
operations, and audit trail. The desktop runs it behind a sidecar adapter for
independent lifecycle, protocol-version checking, and a smaller ambient-authority
surface than the complete desktop or server. Process separation is useful
defense in depth, not a substitute for authorization: the broker validates every
operation even when its caller is local.

The sidecar adapter is a small `openwave-host-broker` process owned by the
desktop host. The desktop builds and bundles it by default, starts one lazy
process for the application profile, and restarts it after a bounded transport
failure. The host supplies absolute app-data and home-directory paths at spawn
time; the broker never guesses them from an agent request. Its stdio wire format
is strict, bounded newline-delimited JSON with an explicit control or operation
channel and exactly one safe response per non-empty input line.
Oversized lines are drained without unbounded allocation so the next request can
still be parsed; whitespace-only or malformed non-empty frames receive a safe
transport error rather than hanging a request/response client. Root counts,
display names, directory results, and file bytes are bounded before response
serialization, so the response cap is also an allocation bound. The app-private
broker directory is added to root policy as a protected location, so it and any
ancestor containing it cannot be connected. Queue admission is bounded and
fails fast when full, and the desktop runs as a single instance so two native
shells cannot contend for the same broker state.

The Tauri host—not the renderer—owns the control handle. It opens the native
folder picker, resolves the renderer's chat ID against the server's authoritative
store, derives the trusted ownership subject from that record, and forwards the
picker result. Current desktop chats use their conversation subject; dormant
project-scoped API chats retain their legacy project subject. Re-registering the
same pinned filesystem object reuses the host-approved root. The host can return
bounded safe summaries of approved roots to the management UI; it never returns
their paths. Roots whose safe display names collide are omitted so a native
confirmation never asks the user to distinguish two folders with the same
label; those folders remain available through the picker. Reusing an approved
root in another chat requires a native confirmation, after which a separate
product attachment change confirms the exact conversation attachment before
the renderer sees it as connected.

The **renderer** presents connected folders and permission choices. It receives
safe summaries, not the broker's persisted absolute-path registry.

## Two channels, not one privileged API

Broker requests are divided into two typed channels:

1. **Control requests** record trusted user actions: negotiate a protocol
   version, list safe summaries of host-approved roots, register a folder
   returned by the native picker, attach or detach an existing root for one
   conversation, or revoke access. Only the desktop host or an equivalent
   explicit local CLI action may send them.
2. **Operation requests** come from agent execution. The current protocol can
   list connected roots, list a directory, read a file as bounded UTF-8 text,
   and read a file as bounded opaque bytes; writes and confined commands are
   later capability slices. Every operation that can expose a host resource is
   denied unless a matching grant exists; an unattached conversation may safely
   receive an empty root list.

The two read shapes carry the same read capability, because the user consented
to reading files below a root rather than to a particular encoding. They differ
in who the bytes are for. A text read returns content the agent will see, so it
is small and refuses anything that is not valid UTF-8. A binary read returns
opaque bytes for a trusted native handoff into a product pipeline, so it allows
the megabytes a PDF or Office document routinely occupies and makes no claim
about the content. Those bytes are base64-encoded on the sidecar pipe, and the
transport's response bound is derived from the binary read bound so a
maximum-size read can never be rejected as an oversized frame. Nothing in this
path lets an agent read the bytes it moved; to see the content it must go
through whatever the receiving pipeline exposes.

Code should expose different controller and operator interfaces so an agent
executor cannot accidentally call the control channel. The broker still
validates every request; the type split is defense in depth, not the only check.

Attaching and detaching are exact conversation mutations. Attaching a
host-approved root installs only the grant needed by that product subject and
the attachment for the selected conversation. Detaching a root from one
conversation leaves the host approval, its grants, and every other
conversation's attachment intact. Revocation is deliberately broader: it
forgets the root and removes all of its grants and attachments. Both mutation
kinds bind a stable operation identity to their original request, including the
trusted consent method for an attach, so an exact retry is idempotent and
identity reuse with different inputs is rejected. The broker stamps new
subject grants with that current attachment consent instead of copying another
subject's earlier consent record. A read-only attachment receipt lets a
recovering native client distinguish unknown, completed, and failed work
without starting or replaying the mutation. Attachment changes are computed and
published in one durable state replacement, so they do not expose a recoverable
intermediate phase.

Legacy version-2 state may contain multiple product root IDs for one pinned
filesystem object. Those IDs remain valid so existing product projections and
receipts do not break. Registration selects among them deterministically, and
global revocation reports no change while an equivalent alias remains instead
of claiming the physical approval was removed.

The desktop's manual Disconnect action uses this conversation-only detach; it
does not invoke global revocation. The connected-folder list is the intersection
of the chat's pathless product projection and the broker's live safe summaries,
so neither a broker-only registration nor a product-only intent is presented as
converged access.

Manual picker registration has a separate app-private receipt because it begins
before product intent exists. The receipt contains the selected absolute path,
original broker subject, and three distinct operation identities; none are
serialized to the renderer. Recovery never starts a registration that was not
already marked attempted. Once product begin commits, the server-owned pending
change is the authoritative recovery queue for both attach and detach.

The product database coordinates its side of the operation separately. It
stores an `awaiting_broker` change before native code talks to the broker, then
finishes it as `completed` or `failed` from an exact broker receipt. Only one
change may wait for the broker per conversation. Attach records product intent
first and rolls it back if the broker fails; detach leaves the root visible until
the broker confirms it is detached. The final projection and terminal receipt
commit together. A crash therefore leaves durable work that a native reconciler
can resume instead of an ambiguous half-update.

The store derives the broker subject from the locked chat. Current desktop chats
use the conversation subject; a chat created through the dormant project API
continues to use its project subject. Callers provide only the stable operation
ID, chat, executor, root, action, expected revision, and creation time. They
cannot choose a project, path, projection position, or provenance.

The native reconciler must derive its stable executor identity from its private
authenticated session; renderer input cannot choose or learn that identity. It
must also bind a broker receipt back to the persisted operation ID, subject,
conversation, root, and action before constructing a product terminal result.
An unknown attachment receipt may dispatch the exact fingerprinted mutation. A
matching completed or durable failed receipt may finish the product change. A
contradictory receipt returns `BrokerStateMismatch`, leaves the change pending,
and must be escalated rather than converted into an ordinary failure.

The host-access context and the conversation's attached-root set are supplied by
trusted conversation execution state, not by model-generated tool arguments.
This stops a model from selecting a different conversation's context—or a
dormant project subject—even if it guesses an identifier.

### Importing an approved file as a conversation source

Reading and importing are different acts, and the tool surface separates them.
`read_connected_file` returns bounded UTF-8 to the model. `import_connected_file`
returns nothing readable at all: it moves the file into the conversation's own
source pipeline and hands back a document id, after which the agent reads it
through the ordinary source tools. That is what makes a PDF or Office document
under an approved root reachable without asking the user to find the same file
again in a picker.

The model's arguments stay the same shape as a read — an opaque root ID and a
bounded root-relative path — and everything consequential is decided natively:

- The broker performs the bounded binary read under the server-derived
  conversation context, checking its own attachment and grant and reauthorizing
  before releasing bytes.
- The attachment is checked a second time immediately before the source is
  published, because publishing is a separate later effect. A detach or
  revocation that wins that race discards the bytes rather than persisting
  them.
- The media type is determined from the bytes, not from the path the model
  named, so a model cannot choose which parser runs by choosing a filename.
- The source identity is derived from the exact conversation, opaque root, and
  root-relative path. Importing the same file again converges on the same
  single source, so a retry after an ambiguous response recovers one source
  rather than adding a second. That is also why an interrupted import may be
  replayed while an interrupted read may not: a read has no durable outcome to
  reconcile a second attempt against.
- Nothing persisted or returned contains an absolute host path. The stored
  source URI uses the same opaque-root plus bounded-relative-path vocabulary as
  the audit trail, and the renderer's catalog projection excludes it entirely.

Because import produces a source rather than text, a file whose parser produces
nothing is reported as stored but not searchable rather than as ready, so the
agent does not describe a scanned document as one it has read.

## Agent-requested access

Agents need a safe way to ask for a folder they cannot currently see. This is a
two-step product workflow, not a privileged broker operation:

1. The agent records an access request with a stable identity, a user-readable
   reason, untrusted proposals for the capabilities it needs, and an optional
   well-known folder hint for `Documents` or `Downloads`. The contract does not
   accept free-form labels or paths.
2. The desktop renders a request card. Only after the user accepts that card does
   it open a native folder picker. A hint may choose the picker's starting
   location, but it confers no authority. Durable coalescing and rate limits keep
   an agent from creating repeated modal prompts.
3. If the user selects a folder, the trusted desktop host derives and displays a
   fixed low-risk capability set, then sends a control request containing the
   picker result and that host-defined set. It never copies the agent's proposed
   capability list into a grant. The broker canonicalizes the root, applies
   policy, and records only those capabilities. Command execution, destructive
   writes, and other high-risk access require their own explicit permission
   dialogs.
4. After broker registration is confirmed, the desktop durably records a fresh
   root-attachment change identity, the observed conversation attachment
   revision, and the immutable creation time before beginning product work. It
   then reconciles the same-ID broker `AttachRoot`, finishes the product change,
   and verifies that the final chat projection contains the root. Only that
   converged state resolves the agent request as connected.
5. The original operation can retry against the returned opaque root identity.
   Cancelling or rejecting the picker resolves the request without any grant.

The control plane should persist the request and its resolution so a restart or
ambiguous response does not create duplicate prompts or grants. The broker
control remains idempotent under its own stable operation identity. Agents never
receive an API that accepts an absolute path, and they cannot silently connect a
standard folder by naming it.

Root policy intentionally refuses the user's entire home directory while
allowing specific children such as `~/Documents`, `~/Downloads`, or a nested
project folder when the user selects them.

## Capabilities and paths

Grants are deny-by-default and scoped to one access context. The initial useful
capabilities are:

- discover connected roots;
- list and read files under a connected root;
- import selected file bytes into OpenWave's private data plane;
- write under a connected root with an explicit, bounded policy;
- later, run commands inside an OS-confined environment.

Capabilities should remain separate. Choosing a folder can grant low-risk file
access, but command execution requires a permission action that names the added
risk. Revocation must take effect at the broker, rather than relying on stale UI
or server state.

Root identifiers are random host-local identities rather than hashes of absolute
paths. This avoids leaking that two product contexts refer to the same machine
location. Operations address `{root_id, relative_path}`. The broker canonicalizes
a root when it is registered, rejects dangerous or overly broad roots, rejects
absolute and escaping relative paths, and defends against symlink traversal at
the time of use. A lexical `starts_with` check is not a security boundary.
Cached directory handles never become grants by themselves; every operation
rechecks live authorization so revocation takes effect immediately.

Read access is privacy-sensitive even though it does not mutate the filesystem:
file bytes may become model input and leave the machine when a hosted provider is
selected. Permission UI should make that consequence understandable.

## Data movement

Broker-private scratch space and user-connected roots are different data
planes. Imported bytes should cross the process boundary through bounded,
by-reference handoffs rather than being placed in agent-addressable scratch.
Outbound writes should similarly originate from a broker-confined staging area
or bounded inline payload. Keeping handoffs separate prevents composing an
import from one root with a write to another root into an unintended
cross-folder copy primitive.

Every machine-touching operation is audited with its access context, capability,
target summary, outcome, and authorizing grant. Stable operation identities and
receipts are required before ambiguous mutations can be retried safely.
Security-sensitive writes and command execution must fail closed when their
intent cannot be recorded durably. App-private scratch may allow replacement;
connected roots default to additive, no-clobber writes, with replacement and
deletion modeled as distinct higher-risk actions.

The local audit is structured JSONL under the broker-private data directory. It
records opaque root IDs and bounded root-relative paths, never absolute host
paths, raw OS errors, or file contents. Each append is synced before returning;
the active file and one previous generation provide bounded local retention.
Transient append failures retry only after the partial attempt has been rolled
back and synced; an ambiguous append or rotation degrades the sink until restart,
where an incomplete tail and interrupted rotation are repaired before new
records are accepted. Unix rotation syncs the containing directory and Windows
uses write-through file moves. Read-tier audit initialization or append failure
is reported locally but does not prevent the user from reading their own files.
Future write, command, and computer-control operations must durably record a
bounded intent before acting and refuse the action if that record cannot be
written. Forwarding this local trail off-device is a separate, explicit privacy
decision, not a side effect of choosing a hosted model.

## Deployment evolution

The trust boundary does not move when deployment changes:

| Deployment | Product control plane | Host consent and I/O |
| --- | --- | --- |
| Local desktop | In-process local server | Tauri host + local broker |
| Self-hosted | User-operated service | Desktop host + broker on each user's machine |
| Managed | Hosted OpenWave service | Desktop host + broker on each user's machine |

Only the transport between the control plane and desktop changes. The broker
continues to accept the same typed operations, keep grants on the user's
machine, and require local consent controls. Provider routing, turn state, and
document state remain control-plane concerns.

Headless mode needs an equally explicit consent action. An absolute path in an
ordinary HTTP request is not consent. A future interactive local CLI command can
register a root for a chosen access context; automation can use deliberately
provisioned grants with clear operator intent.

## Implementation slices

This will land in independently reviewable pieces:

1. Add `openwave-host-broker` with typed capabilities, grants, access-context and
   root identifiers, authorization value-model/specification tests, and
   descriptor-pinned root policy.
2. Add the versioned control/operation protocol and an owning in-memory broker
   that authorizes pinned handles, performs bounded-result I/O, and reauthorizes
   before releasing bytes so completed revocation fences in-flight results:
   hello, register/revoke/list roots, list directory, and read file as text or
   as opaque bytes.
3. Persist the root/grant/attachment registry and idempotency receipts atomically
   under a broker-private, exclusively owned state directory. Restart must
   validate the bounded state file and revalidate and descriptor-pin every
   persisted root before advertising it. A mutation with ambiguous publication
   fails the broker closed until restart.
4. Add bounded audit records for every machine-touching operation, including
   the exact grant that authorized an operation, de-sensitized targets, and
   local two-generation retention.
5. Expose the same contract through a bounded, versioned sidecar adapter with
   strict control/operation wire variants and app-private-directory protection.
6. Add the native Tauri sidecar lifecycle and connected-folder UI. The renderer
   can request only pick/list/revoke for its current conversation; it never
   receives a raw control surface or absolute path. Broker state, audit, and
   scratch live under the protected OpenWave application-data directory.
7. Add the durable agent-request → native-picker → grant → retry workflow. The
   generic tool-execution foundation is now present: canonical requests are
   immutable, client work is durably discoverable, and per-claim fencing tokens
   guard heartbeat, terminal resolution, and explicit expired-claim recovery.
   Authenticated per-chat pending/claim/heartbeat/resolve routes now expose that
   state machine without leaking tokens through polling. Atomic turn parking,
   cumulative progress accounting, and the agent/worker handoff for a singular
   client-owned call are now implemented. The bounded `request_folder_access`
   contract is registered without a server executor; its capability list is
   explicitly an untrusted proposal. The broker exposes a de-sensitized,
   read-only registration-receipt lookup for crash recovery; looking up an
   operation can never start or resume it. This lets a restarted client resolve
   a known, still-connected registration without replaying a mutation after
   losing its client lease. Lookup is fenced by the original trusted subject and
   conversation and reports a later disconnection separately from a usable
   grant. The desktop now polls that authoritative pending-work boundary, shows
   a bounded consent card, and keeps the picker, claim token, selected path, and
   broker mutation inside the native process. App-private receipts preserve the
   exact registration operation ID, claim token, intent, product change ID,
   attachment-revision fence, and terminal payload across a crash. These
   identities are separate from the tool call and from each other. The private
   receipt also keeps the bounded root display summary and a distinct exact
   cleanup operation, so product recovery no longer depends on registration
   remaining connected.
   A pre-effect phase is synced before the one registration dispatch, and the
   dispatch must begin inside a short post-heartbeat deadline. Recovery runs in
   the background with bounded backoff; it may query the broker receipt and
   publish a known result, but an attempted receipt never starts or replays
   registration. A confirmed registration now begins and finishes the durable
   product root-attachment state machine through the private native routes;
   exact broker attachment recovery is lookup-first, and `connected` is held
   until a final product projection read matches the terminal receipt. A
   permanent product begin rejection drives a same-conversation `DetachRoot`
   cleanup before resolving failure; it never broadens that cleanup into
   project-wide revocation. If that detach itself reports a durable failure
   because another trusted action already revoked the root, recovery accepts
   cleanup only after the original registration receipt authoritatively reports
   the same root disconnected. A historical attach receipt whose live state is now
   detached finishes product work as failed and rolls back its provisional root.
8. **Pathless baseline complete:** project/chat `workspace_dir` is gone. The
   pre-v1 schema stores bounded ordered opaque root projections and revisions,
   while runtime-only legacy scratch is derived under private server data and
   never returned by the product API. The broker now supports exact,
   idempotent per-conversation attach/detach plus read-only recovery receipts.
   The durable product-side attachment operation is implemented in core and
   used by the agent-approved folder-consent path. Manual connect/disconnect
   controls and startup-wide reconciliation remain separate slices.
9. Route built-in file tools through the operation interface and remove direct
   ambient host-directory opening from `ToolCtx`.
10. Port bounded imports, writes, approvals, and confined command execution as
   separate capability slices.
11. Adapt the explicit-workspace MCP command to the same broker policy instead of
   maintaining a second filesystem authority model.

Each intermediate state must fail closed. In particular, adding the data model
must not expose roots before tools use the broker, and adding broker code must
not imply that the current direct-workspace tools are already safe for untrusted
paths.
