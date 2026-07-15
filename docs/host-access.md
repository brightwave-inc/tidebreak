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
folder picker. Different projects or conversations can resolve to different
host-access contexts, and each context can contain more than one connected root.

In practical terms:

- OpenWave may always use its own private application data and scratch space.
- A project can have its own ordered set of connected folders.
- A standalone conversation can have its own ordered set of connected folders.
- A conversation in a project can use project roots plus an explicit attached
  subset or conversation-specific roots. Adding a project root must not silently
  widen an already-running turn.
- The model sees an opaque root identifier, a display name, and root-relative
  paths. It never receives authority by naming an absolute host path.
- Connecting one folder never grants access to its siblings, the user's whole
  home directory, or another project's roots.
- An agent can request access to another folder, including familiar locations
  such as Documents or Downloads. The request can explain why and suggest where
  to open the picker, but only the folder the user actually selects is granted.

The current `workspace_dir` fields on projects and chats do not express this
model. They are temporary pre-alpha implementation details and will be replaced
rather than preserved as a compatibility layer.

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
store, derives project or standalone ownership from that record, and forwards
the picker result. Re-registering the same pinned filesystem object for the same
subject reuses the root and only adds a missing conversation attachment.

The **renderer** presents connected folders and permission choices. It receives
safe summaries, not the broker's persisted absolute-path registry.

## Two channels, not one privileged API

Broker requests are divided into two typed channels:

1. **Control requests** record trusted user actions: negotiate a protocol
   version, register a folder returned by the native picker, inspect grants, or
   revoke access. Only the desktop host or an equivalent explicit local CLI
   action may send them.
2. **Operation requests** come from agent execution: list connected roots, list
   a directory, read a file, import bytes, write allowed output, or later run a
   confined command. Every operation is denied unless a matching grant exists.

Code should expose different controller and operator interfaces so an agent
executor cannot accidentally call the control channel. The broker still
validates every request; the type split is defense in depth, not the only check.

The host-access context and the conversation's attached-root set are supplied by
trusted conversation execution state, not by model-generated tool arguments.
This stops a model from selecting a different project's context even if it
guesses an identifier.

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
4. The original operation can retry against the returned opaque root identity.
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
   hello, register/revoke/list roots, list directory, and read file.
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
   exact operation ID, claim token, intent, and terminal payload across a crash.
   A pre-effect phase is synced before the one registration dispatch, and the
   dispatch must begin inside a short post-heartbeat deadline. Recovery runs in
   the background with bounded backoff; it may query the broker receipt and
   publish a known result, but it never starts or replays registration.
8. Replace project/chat `workspace_dir` with persisted host-access context
   identity and connected-root APIs. This can update the pre-v1 baseline schema
   directly.
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
