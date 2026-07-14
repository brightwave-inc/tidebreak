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
   folder hint such as `Documents`, `Downloads`, or a project name.
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
3. Persist the root/grant/attachment registry and idempotency receipts atomically,
   add bounded audit records, and expose the same contract through a sidecar
   adapter. Restart must revalidate and descriptor-pin every persisted root before
   advertising it.
4. Add the Tauri sidecar client, connected-folder UI, and the durable
   agent-request → native-picker → grant → retry workflow. Broker state, audit,
   scratch, and handoffs live under OpenWave's application data directory;
   remove the automatic `Documents/OpenWave` folder.
5. Replace project/chat `workspace_dir` with persisted host-access context
   identity and connected-root APIs. This can update the pre-v1 baseline schema
   directly.
6. Route built-in file tools through the operation interface and remove direct
   ambient host-directory opening from `ToolCtx`.
7. Port bounded imports, writes, approvals, and confined command execution as
   separate capability slices.
8. Adapt the explicit-workspace MCP command to the same broker policy instead of
   maintaining a second filesystem authority model.

Each intermediate state must fail closed. In particular, adding the data model
must not expose roots before tools use the broker, and adding broker code must
not imply that the current direct-workspace tools are already safe for untrusted
paths.
