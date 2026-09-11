# Slack sessions

Status: proposed. This page is the working design. It is not a decision
record. Where a later record disagrees, the record wins. This revision
follows an adversarial review pass; the largest changes are recorded in
"Bets and exit criteria" and "Open questions".

No repository is required to start. A repository-less Slack session with no
explicit harness uses the channel's harness preference, then the configured
runtime's admitted default engine. Without a runtime, or with a legacy runtime
that declares no engines, it uses Internal on the machine. An explicit Internal
choice also stays on the machine. Invalid runtime settings refuse admission
instead of silently changing the execution location.
See [Repository-less sessions](#repository-less-sessions) and the amendment to
[decision 94](decisions/0094-repository-optional-conversations-on-the-internal-engine.md).
Durable child wait/resume and the thread/web tree remain follow-up work.

A person talks to Tidebreak in Slack — in the agent's own chat (Slack's
primary and split view for AI agents) or in a channel thread. Tidebreak
runs a session for that conversation on that person's hosted machine.
The session runs in a configured sandbox or on the machine, according to its
selected harness and execution location. Progress returns to the conversation
through Slack's agent surfaces. The same session appears in the desktop inbox.

Version one ships both surfaces: the agent DM and channel threads.
Getting the agent into a conversation uses Slack's own affordances —
"Add agent", the channel's Agents & apps tab, @mention — and none of it
is custom chrome. No repository is required to start. A conversation
with no repository can answer and discover repositories; a task that
names repositories can start child sessions in independent workspaces under
the same conversation, and the conversation can wait on those children
and read their results. Grant-bound children follow the configured external
placement. Remaining adapter and orchestration work is recorded below.

## Opening channel sessions on the web

The adapter sets session visibility through the workspace grant's bound
`PUT /external/code/sessions/{id}/access` route. The optional `visibility`
field accepts `deployment` or `private`. Contributor replacement and the
visibility change commit together. Older adapters omit the field and preserve
the session's visibility.

Only channels that the adapter verifies as public and not externally shared
may use deployment visibility. Private channels and unknown channel metadata
stay private. Deployment visibility lets signed-in people read the session;
it does not grant permission to send instructions or edit the workspace.

The workspace list includes workspaces that contain a session the caller may
read. Shared entries carry `read_only: true`. Their cards open the transcript
without offering workspace commands or requesting owner-only Git status.
Revoking the session's access removes the workspace from that reader's list.

## What is true today

- Desktop, CLI, mobile, and `tidebreak agent-mcp` speak one attach
  contract: HTTP plus WebSocket
  ([`0012`](decisions/0012-data-dir-listen-endpoint.md),
  [`0072`](decisions/0072-mobile-client.md),
  [`0073`](decisions/0073-agent-mcp-drives-chat-over-attach.md),
  [`0074`](decisions/0074-agent-mcp-drives-code-mode.md)). Workspace
  create, session create, submit turn, steer, interrupt, session events
  over WebSocket, and reap all exist. The external-session endpoints in
  this design are new.
- A send while a turn runs queues as a durable FIFO row and promotes at
  the turn boundary. Steer is a separate, explicitly requested act, gated
  per harness by a capability probe, guarded by `expected_turn_id`, and
  lands only at a tool boundary even where supported. This design keeps
  that contract; it does not invent steer-by-default.
- A code session may bind no workspace: `code_session.workspace_id` is
  nullable, `repo_id`/`worktree_path`/`branch_name`/`base_ref` on a
  workspace remain non-null, and the runtime refuses workspace-bound
  operations on a workspace-less session. Multiple repository workspaces
  hang off one conversation as children, and the internal engine can
  discover, create, drive, and wait on them with the native self-drive
  tools. A workspace still assumes exactly one repository; multi-
  repository workspaces are deferred.
- `tidebreak-supervised-agent` clones each declared repository in order
  and runs in the first clone. WIP refs are
  `mg-wip/<sandbox-id>-i<incarnation>` for the first clone,
  `-r<position>`-suffixed for later ones. An empty repository list is a
  research run: no clone, no WIP push, and the deliverable leaves as a
  `task_output` event body (224 KiB cap) emitted only at supervisor stop —
  not per turn. Repository-less internal sessions stream their assistant
  text per turn through the normal code event surface, independently of
  the supervised sandbox agent.
- Hosted git borrows a short-lived GitHub credential per operation. The
  credential is the person's user access token; it cannot be narrowed to
  one repository. Its ceiling is the GitHub App installation intersected
  with the person's own access
  ([`0063`](decisions/0063-hosted-machines-borrow-forge-credentials.md),
  [`0065`](decisions/0065-hosted-git-acts-as-the-person.md)). Every trust
  statement in this design is made with that unscoped token in mind.
- Remote session execution now provisions sandboxes, tracks incarnations,
  ingests events, resumes from pushed WIP refs, and reaps fenced sessions.
  Trigger turns and image attachments remain typed refusals because the
  runtime contract cannot preserve their delivery rules; Execution records
  those product boundaries.
- The environment that provisions and confines the sandbox is external to
  this repository. Tidebreak calls its spawn, steer, and event APIs and
  requests ceilings at spawn; it does not own enforcement of them. Naming
  and versioning that contract is a dependency of stage 1, not a detail.

## Hosted machine connections

On a gateway-managed machine, a confirmed Slack connection retains a gateway
delegation. The gateway stores the consent. Tidebreak stores the connection
identifier and uses the machine's existing gateway identity to request
short-lived tokens. Browser closure, browser token expiry, and a machine
restart do not require another sign-in for the connection's machine sessions.
Tidebreak checks the live local grant before each delegated inference, catalog,
and Git credential request. Revoking or replacing the connection denies local
access immediately. A retry sweep completes gateway revocation after an outage
or restart. Revoking the source gateway sign-in also ends its delegation.

Connections approved before gateway delegation support need approval again.
A live browser sign-in cannot supply credentials for those old connections.
When Slack reports that approval is required, send "reconnect" to Tidebreak and
approve the connection. This replaces the old grant; existing sessions keep
their original grant and do not acquire the replacement's authority.

## Bets and exit criteria

Slack as a channel is a product conviction, not a bet under test: people
want their agents where their conversations are, and Slack is building
first-class agent surfaces to meet them. What stays instrumented is the
shape inside the channel:

- Replies that arrive while a turn is running. Steer ships only if this
  demand appears (as a starting gate: a quarter of sessions receiving
  one).
- Repository-less asks. Scratch ships only if people demonstrably try
  them.
- Sessions ending in a merged pull request, and sessions per user per
  week, so the investment conversation stays honest.

## Design for leverage

Decisions here are made so the things we know are coming — memory,
dissolving the wall between chat-shaped and code-shaped work
([`0048`](decisions/0048-one-interaction-model.md) step 5), more
channels — land as additions, not re-plumbing:

- The machine's binding table is channel-agnostic. It maps an opaque
  `(channel kind, external key)` to a session; Slack's thread key is one
  kind. GitHub issue or PR-comment driving, Teams, or email later reuse
  the same external-session endpoints, the same idempotency contract,
  and the same grant shape with a different adapter in front.
- The adapter is session-kind-agnostic. It binds a conversation to a
  session and renders attention and events; it never encodes "this is
  code mode". When the interaction models merge, the Slack surface does
  not change.
- The journal carries a per-turn assistant record from day one (see
  Rendering and stage 1). That record is what Slack streams, what the
  desktop pickup shows, and the substrate a memory system will mine.
  Retention is designed as durable session history, not render cache.
- Prefer Slack's native agent surfaces over custom chrome: the agent
  chat and History tab as the session list, thread titles set from the
  session title, Thinking Steps for progress, and — as a fast follow —
  Slack Code channels, whose "working / needs your attention" status is
  Tidebreak's attention model rendered natively. The less custom UI the
  adapter owns, the more Slack's own investment compounds for us.

## Vocabulary

| Noun | Meaning |
| --- | --- |
| adapter | The shared Slack service. It verifies Slack webhooks, durably queues them, maps them onto attach calls against the owner's machine, and renders session events back into the thread. It runs no engine and keeps no session journal — but it is stateful, and this design says exactly what state it owns. |
| grant | One Slack user's link to one Tidebreak principal: the machine reference plus an adapter-scoped token that machine minted. |
| thread key | The durable Slack-side identity of a conversation: `(enterprise_id or team_id, channel_id, thread_ts)` for a channel thread or an assistant-chat thread; `(enterprise_id or team_id, channel_id)` for a classic DM or group DM. Stored machine-side as one kind of opaque `(channel kind, external key)` binding. |
| session | One durable conversation with one harness inside a workspace. Owner-scoped. |
| incarnation | One sandbox lifetime within a session. Sessions outlive incarnations. |
| artifact | What the thread receives when a turn settles: a pull request link, or a compare link for pushed WIP with a one-line summary. |

## Slack is a Tidebreak client

The adapter maps Slack events onto the attach contract, using the attach
client directly the way `agent-mcp` does internally. It does not drive an
engine, provision a sandbox, decide session lifecycle, or keep a session
journal. Every decision that depends on session state lives on the
Tidebreak machine, because the adapter's view of state is always stale.

Rejected: a stateless adapter. Slack's delivery contract (200 within
three seconds, three retries over about six minutes, at-least-once, no
ordering) and Slack's habit of disabling misbehaving endpoints force the
adapter to own durable state. Pretending otherwise just moves the state
into bugs.

Rejected: putting session logic in any service other than Tidebreak.

## The adapter is a shared, stateful service

A Slack app has one event URL. Hosted machines are per-person
([`0047`](decisions/0047-gateway-linked-hosting.md),
[`0072`](decisions/0072-mobile-client.md)). So one adapter serves many
principals on many machines, and it is a multi-tenant credential
custodian. Design and operate it as one.

Deployment: the adapter runs as a gateway add-on, the same pattern
Shipright uses — not a new service class. It is self-hostable, so an
organization can run its own Slack app against its own machines; the
hosted instance is a convenience, not the only path.

The adapter durably stores:

- Grants: `(enterprise_id or team_id, slack_user_id) → (machine
  reference, encrypted adapter token, workspace identity)`. Machine
  references come from the hosted-machine registry, never from user
  input; the adapter resolves and TLS-pins them per grant. The adapter's
  own Slack bot tokens, one per installed workspace, live in the same
  custody regime.
- The thread-routing table: `thread key → (machine, session_id, state)`.
  This table is authoritative on the adapter — it is the thing that
  decides which machine owns a thread, so no machine can rebuild it. It
  is written with a pending-row protocol: write `(thread key, machine,
  pending)` before calling get-or-create, finalize after. On a routing
  miss for a thread where the adapter's own bot has already posted a
  status message, refuse and alert rather than create a second session
  under a different owner.
- The inbound event queue (see Ingestion).
- Per-session render state: the status message `channel` and `ts` for the
  open turn, the last consumed event sequence, the artifact-posted-through
  turn ordinal, and per-(thread, user) notice flags. Every render action
  must be resumable from this row; an adapter restart neither double-posts
  nor orphans a status message.
- Channel repository defaults, with who set them and when.

Token handling: adapter tokens are minted by the machine with an
adapter audience and these scopes only — external get-or-create, submit
messages, watch events, interrupt, and reap, each restricted machine-side
to sessions tagged with that grant's id. No settings, no repository
administration, no other sessions. Rotation uses refresh tokens with
reuse detection: a replayed rotated token means theft — revoke the grant
and notify the owner. The machine stores only a hash of the adapter
token. Keys that wrap stored tokens live in a KMS, not beside the data.
Revocation is immediate: it severs live event WebSockets and interrupts
delivery for that grant's sessions, not just future calls. The desktop
settings and the Slack App Home both list active grants with a revoke
control, including revoke-whole-workspace.

The pre-grant connect route is not anonymous. Each machine accepts one or
more operator-provisioned adapter bootstrap bearers. The shared adapter keeps
the active bearer with that machine's operator-written directory entry and
uses it only to start a handshake. The machine returns a separate 15-minute
confirmation capability, which the adapter encrypts with its token-vault key
before it stores the pending row and presents on status and completion. This
keeps an arbitrary caller from manufacturing a convincing approval and keeps
a forwarded approval link from completing without the adapter.

A compromised adapter can submit prompts to sessions and read their events under
its connected grants. A workspace grant can select any repository allowed by the
instance's configured GitHub App. Approving that workspace connection grants this
shared authority to its admitted channels. GitHub App installation permissions,
grant revocation, and adapter membership checks define that boundary.

## Identity and connect

A session acts as one forge identity, chosen when it starts, the same way it
chooses where the engine runs. Hosted git names that choice on every borrow,
and the gateway answers or refuses by name.

A DM session defaults to acting as the person when the gateway offers their
identity. When it does not, the session runs as the bot, says so, and offers
a Connect link. A person may ask for `bot` explicitly. A session owned by a
service principal always acts as the bot; the body's `acts_as` is ignored.

Get-or-create decides this once, before the workspace is cloned, so the clone
borrows the same identity the session will. The response names `acts_as`,
`acting_login`, `app_name`, and `connect_url` (only when the person could
connect and did not) so the adapter can say who is acting and how to connect.

A Slack user does not run until they hold a grant. Channel sessions run
under a workspace grant instead: a service principal starts that
handshake, an admin approves "run channel sessions for Slack workspace T
as tidebreak-slack", and the adapter completes it the same way.

On first mention from an unmapped user, the adapter stores the pending
message against a one-time handshake nonce and posts an ephemeral connect
card — never a public channel post, and never wording that announces the
person's unconnected state to the room. The link opens a connect approval
on the person's hosted Tidebreak surface. The approval page shows the
Slack workspace name, the display name, and the avatar being linked, and
asks "is this you?"; the POST is CSRF-protected. After approval the
adapter DMs the Slack user a confirm button, proving control of the Slack
account and not just possession of a link — a forwarded connect link
therefore binds nothing. On completion the adapter submits the stored
message and posts "Connected — starting on your request" in the thread,
so the person never retypes.

This flow requires a hosted surface that can render an approval. The
launch audience is therefore people with a hosted machine and a gateway
login; the approval surface is in stage 2's scope, not assumed. Local
desktop via a relay stays the same follow-on mobile already named.

Trust boundaries, stated rather than implied:

- A hostile Slack workspace admin can, through IdP and session control,
  act as any member of that workspace. A grant is trust in the person
  and their workspace's administration. The grants list shows the
  workspace so an owner can revoke a whole workspace at once.
- The adapter subscribes to `user_change`, deactivation-relevant, and
  install-lifecycle events. Grants fence for re-confirmation when the
  linked user's email or identity changes, and all of a workspace's
  grants fence on `app_uninstalled` or `tokens_revoked`.
- On Enterprise Grid, identity resolves from the event's `authorizations`
  context, keyed by `enterprise_id` where present; a Grid migration
  fences the workspace's grants for re-connect rather than guessing at
  remapped IDs.

A machine session's own `git` and `gh` borrow the session owner's forge
credential per call through the machine's loopback route, so the engine
never holds a token. When that borrow is refused, the helper prints
`Tidebreak: git credential refused (<status>): <reason>` to the engine's
stderr and the machine journals a `credential_refused` event carrying the
reason class (`connection_ended`, `not_connected`, or `forge_refused`),
the message, and the remedy. The desktop shows it as a notice on the
session; the adapter posts it in the thread, with the connect card when
the connection ended, so a push that stops is never a bare authentication
failure nobody can act on.

The configured GitHub App installation defines the shared instance's repository
access. Every connected channel inherits that access. A person grant uses that
person's forge access. A workspace grant uses the deployment credential the
service principal already uses (decisions 89 and 96).

Only the session owner's messages reach a person-grant session. Anyone
else's reply gets an acknowledging reaction from the bot and, once per
user per thread, an ephemeral notice that says what they can do: who the
owner is, that the agent does not read the thread, and how to connect
themselves. Person-grant sessions refuse quoted thread context. Under a
workspace grant, the adapter sends the actor on each message and mirrors
private-channel membership into session access rows (decision 86). A channel
that opts in may supply prior messages from full home-workspace members as
quoted context on the session's first message.

## Thread and session

One conversation is one Tidebreak session, across both v1 surfaces:

- In the agent's own chat (primary view and split view), every "New
  Chat" Slack creates is a distinct assistant thread with its own
  `thread_ts` — each is one session, and Slack's Chat and History tabs
  are the native session list. The adapter sets the thread title from
  the session title, so history reads as named work, not timestamps.
- In a channel, the thread under the first @mention is the session. The
  agent gets into the channel through Slack's Agents & apps tab; the
  adapter builds nothing for that.
- A classic DM message outside the assistant container falls back to
  the conversation-keyed mapping; the lone word `new` ends it and the
  next message starts fresh. Group DMs follow channel rules for
  ownership and disclosure.

Slack Code channels — a dedicated space per piece of work, with an
agent, teammates, and a native "working / needs your attention" status —
are the fast follow, not v1: one code channel maps to one session and
`AttentionState` feeds the channel status directly.

The session uses the code-shaped journal and attention model. Repository-less
Slack conversations use this same code session surface, including when Internal
runs on the machine; they do not create a separate chat row.

| Slack | Tidebreak |
| --- | --- |
| First @mention in a thread | Get-or-create the external session; submit the message as the first turn |
| Later eligible contributor reply | Submit to the messages endpoint when automatic replies are enabled and the thread is awake; outcome is `new_turn` or `queued` (see Ingestion) |
| Eligible contributor reply `stop` | Interrupt the active turn |
| Reply while `Fenced` | Owner sees the reason and an owner-only reap button; anyone else sees the fenced notice |
| Reply while `Ended` | Refuse, with the context-correct next step: "send `new`" in a DM, "start a new thread and mention me" in a channel |
| Reply from someone without contributor access | Refuse without submitting a turn |
| Bare @mention with no task text | Prompt for the task; no sandbox spawns |
| Channel @mention outside a thread | The adapter replies in a new thread; that thread is the session |

`stop` and `new` match after trimming and case-folding, as the entire
message. The machine's response names the interpretation ("Stopping the
current turn", "Started a fresh session") so a mis-parse is visible.

Engine-child park
([`0064`](decisions/0064-idle-engine-children-are-parked.md)) is not
this. That park is invisible reclaim of a local process. Here the sandbox
is the child: stop it after a post-turn idle window. The session row
stays; the next message reincarnates. Sandbox lifetime and session
lifetime are different clocks.

## Ingestion

Slack delivers at least once, unordered, and gives up after roughly six
minutes. The pipeline is therefore: verify signature and timestamp,
answer 200, durably enqueue, process asynchronously.

- The adapter accepts exactly: `url_verification`; plain `message`
  events with no `subtype` and no `bot_id`, from a granted user or a user
  who can be offered connect; slash-command and interaction payloads; and
  the identity and install-lifecycle events named earlier. Everything
  else — `message_changed`, deletions, the adapter's own posts echoed
  back, unfurl events — is acknowledged and dropped. An edited message
  never re-submits a turn.
- The queue is keyed by Slack `event_id`, which is stable across Slack's
  retries. The adapter retries delivery to the machine with backoff far
  past Slack's window; when a machine stays unreachable, the thread gets
  a visible failure notice, not silence. One slow tenant machine must
  never stall the webhook endpoint for everyone — the ack path touches
  only the local queue.
- Slash-command and interaction payloads carry no `event_id`; the adapter
  derives a replay key from the payload's `trigger_id`/timestamp and
  treats out-of-window duplicates as replays.

The machine's messages endpoint takes the text, the Slack `event_id` as
an idempotency key, and the Slack message `ts`. Outcomes are explicit:
`new_turn` when the session is idle (reincarnating first if needed) or
`queued` when a turn is running — the shipped queue-default contract,
promoted FIFO at the turn boundary. The endpoint never silently steers;
steer stays a distinct, capability-gated verb and is out of stage 1
entirely. Messages that arrive out of order within a short window are
applied in `ts` order, so "A then B" from the user cannot become "B
steered by A".

Idempotency follows the queued-turn pattern the code already uses: the
`event_id` commits in the same transaction as the queue row or turn row
it caused, and a replay derives its response from that row's current
state. There is no separate outcome snapshot to go stale.

The first message may include a `context` array of objects with `author`,
`timestamp`, and `text`. Context requires a workspace grant,
`context_opt_in: true`, and `context_binding_id` naming a binding owned by
that grant and session. The machine records the consent on the binding in
the same transaction as the input. Later context is refused, including after
the first queued message was retracted; delivery retries retain their original
outcome.

Context contains at most 20 messages and 16 KiB of combined UTF-8 text.
Author names are limited to 128 bytes and timestamps to 64 bytes. Each field
must be nonempty and contain no NUL characters. The machine quotes the
messages as JSON under an untrusted-context notice, then appends the current
request. The turn stores that complete input so the web shows the same text
that reached the engine. The adapter owns channel opt-in and membership
filtering; the machine validates grant scope, binding, bounds, and first-use
rules.

A conflict response of `ended`, `fenced`, or unknown-session is the
defined signal for the adapter to durably close its routing row and
render the refusal. Get-or-create returns `ended` rather than
resurrecting. On a get-or-create hit, the pinned repository wins; a
conflicting spec in the retry is reported, never applied.

## Choosing the repository

A repository selector is optional. On first contact, the adapter resolves it
in this order:

1. A `repo:owner/name` directive in the first message. A near-miss —
   wrong spacing, an inaccessible or misspelled name — refuses loudly
   before anything is created ("Did you mean `repo:owner/name`?"), never
   falls through.
2. Else the channel default. A DM has no default.
3. Otherwise start a repository-less code session. The agent can answer,
   discover accessible repositories, and start repository work when needed.

Bare GitHub URLs in prose are context, never clone intent.

The first use of a channel repository under a person grant keeps its owner
confirmation. Workspace grants use the instance's configured GitHub App directly:
selecting a repository, starting a child, or attaching a task to another channel
requires no channel approval. Setting or changing a channel default posts a
visible notice naming who changed it. Engine action approvals remain separate.

The GitHub App installation intersected with the person's access is the
allowlist, refused with a human-readable rendering: outside the App
installation → "ask an admin to add it to the Tidebreak GitHub App";
no personal access → "you don't have access to `org/name`". Shared channel
sessions use the installation identity's access. There is no separate channel
repository allowlist.

An explicit selector pins the session's workspace; a retry cannot replace it.
A repository-less conversation can create children in several repositories
without changing threads. Each child workspace still belongs to one repository.

## Packaged sandbox runtime

The `supervised-agent` target in `deploy/self-host/Dockerfile` shares the
server image's verified tools and packages Tidebreak's pinned Claude Code and
Codex engines. The release workflow publishes `tidebreak-supervised-agent` beside
`tidebreak-server` for both Linux architectures. It includes the existing
Rust and Python bundles, git, and the GitHub CLI. The supervised image carries
no server, renderer, or machine authentication store.

Gateway starts the image through its BYO contract at
`/usr/local/bin/sandbox-agent`. The workload runs as UID 65532 and receives
only the public `mg-sandbox-placeholder` value. Gateway's sidecar holds the
execution credentials under a different UID and process namespace. Tidebreak
reuses its existing certificate wait, trust bundle, harness adapter, and
`assistant_record` journal ingestion. The browser and desktop therefore
render the same session without a separate Slack transcript UI.

The supervised engine receives Gateway's `SANDBOX_PROXY_ENDPOINT` through
explicit HTTP proxy settings. The endpoint must name a loopback IP address
and a nonzero port. Missing or invalid proxy settings fail before the first
turn; ambient proxy credentials do not pass into the engine.

The machine declares its endpoint and profile through
`TIDEBREAK_RUNTIME_ENDPOINT` and `TIDEBREAK_RUNTIME_PROFILE`.
`TIDEBREAK_RUNTIME_ENGINE` selects the default engine. Without an allowed-engine
list, it remains the only engine the runtime accepts. Managed runtime provisioning
also sets `TIDEBREAK_RUNTIME_ENGINES=claude_code,codex`, so each session can select
one of the two packaged engines when managed registration is enabled with
`TIDEBREAK_RUNTIME_EMBEDDED_ENGINE_REGISTRATION=true`. Existing custom profiles
omit that setting, keep their existing token exchange and spawn behavior, and
continue to use only their declared default engine. Each engine uses Allow permissions inside
Gateway's confinement. Unsupported engine,
permission, or fast-mode settings are refused before a remote turn starts.
The custom-harness contract does not transport tool approvals.
Choose the model and reasoning level when you create the session. This
profile cannot change either setting between messages.

The Gateway deployment declaration attaches the existing general GitHub app
to a dedicated Direct endpoint and grants only the
`runtime:tidebreak` audience and resource with `runtime:execute`. Git and
PR API requests use that app's existing identity and policy. A DM does not
change installation-only credentials into a personal GitHub identity.
Channel sessions use the shared service identity. The channel membership gate
determines who can contribute; each submitted turn retains its actor.

An ordinary custom harness keeps Gateway's generic client identity. A managed
Tidebreak runtime can use an eligible subscription after the registered Tidebreak
instance authenticates the spawn and the supervised agent confirms the installed
engine, version, and session. Gateway validates that confirmation against the
admitted image and the live sandbox before inference starts. Subscription sharing,
provider compatibility, sandbox exclusions, and quota limits still apply. A
missing or mismatched confirmation refuses the run.

A repository-less Slack session also uses the admitted runtime default when
neither the request nor its channel selects a harness. The selected managed
harness can use eligible subscriptions under Gateway policy. Explicit Internal
sessions stay on the machine and do not inherit subscription eligibility from
a child harness. The managed profile limits each sandbox to one pod incarnation;
the image does not restore the harness conversation across pod replacement.

The profile pins a supervised-agent digest; it does not track server releases.
For an upgrade, publish both images from the same release, update the profile
first, and then update the server. The supervised image must accept the prior
task format during this transition. If the server tracks releases, coordinate
its update or pin it until the matching supervised image is ready.

## Execution

For sandbox sessions, `tidebreak-supervised-agent` drives the selected engine
inside a confined sandbox for that session
([`0079`](decisions/0079-supervised-agent-declines-the-sandbox-protocol.md)).
Tidebreak calls the confining environment's runtime API. That contract
is pinned in `crates/tidebreak-server/src/code/remote/`: spawn on
`POST /api/v1/runtime/endpoints/{endpoint_slug}/sandboxes` (preflight
refuses loudly before anything is provisioned), status, a durable
gap-free events cursor with a held wait of at most 25 seconds, inbox
messages, and cancel — each call authenticated with a short-lived
per-owner bearer minted for the `runtime:{endpoint_slug}` resource with
the `runtime:execute` scope, so a sandbox is provisioned as its owner
and never as a shared machine identity. This unparks remote session
execution in
[`deferred.md`](deferred.md); the parking of "deeper isolation" there
concerned detached execution under Tidebreak's own container trust root,
which this path does not use.

A session runs where the deployment can run it
([`0088`](decisions/0088-a-slack-session-runs-where-the-deployment-can-run-it.md)).
The machine chooses an execution location once, at external
get-or-create, from what it has: a gateway sandbox when a sandbox
runtime is configured, else its own engine on a worktree under its
worktree root. That default is for external sessions only. Desktop,
mobile, and `agent-mcp` sessions on the same machine keep machine
execution. The location is stored on the session, reported on the
snapshot and the external event stream, and never changes. After
create, `execution_location` is the only dispatch rule: a later
message, including one typed on the web, follows the stored location.
The machine engine is the floor, not an interim path: a deployment
without a gateway, and a gateway deployment without a configured
runtime, runs Slack sessions the way it already runs desktop, mobile,
and `agent-mcp` sessions. An earlier version of this page refused to
ship Slack on the machine engine; the refusal rested on the sandbox
being the only thing that made unattended `Allow` safe, and the answer
is not to withhold sessions but to withhold `Allow`.

Permission mode follows the location. Inside a sandbox the engine is
`Allow`; confinement is the permission boundary
([`0039`](decisions/0039-allow-is-a-first-class-code-permission-mode.md)).
On the machine the session takes the mode the channel named, else the
operator's default: `TIDEBREAK_EXTERNAL_PERMISSION_MODE` sets that
default (`ask` unless the operator says otherwise) and
`TIDEBREAK_EXTERNAL_PERMISSION_CEILING` bounds what a channel may ask
for with `/tidebreak mode`. A request above the ceiling is refused by
name, `permission_mode_above_ceiling`, so the person learns the
deployment's rule instead of getting a silently clamped session; a
request for anything but `allow` on a sandbox deployment is refused as
`permission_mode_unsupported`. You answer approvals where you are. A
machine session parks on the same cards the desktop shows; the external
stream carries their facts and you settle them from Slack. A sandbox
session stays Allow and never asks, so it still carries none of these
events. Slack `NeedsYou` includes `approval_requested` on a machine
session; connect, fenced, or failed stay as they were.

Incarnations follow a durable intent protocol: write the incarnation
intent row, provision, activate. Stop and reincarnate serialize through
that row — a message that lands while the sandbox is stopping waits
until the stop completes and the dying incarnation's terminal events are
in the journal, so a resume can never miss its predecessor's output and
two incarnations can never run at once. A reconcile sweep cancels
sandboxes whose intent never activated, so a crash between provision and
store cannot leak a spending sandbox. The per-workspace in-memory turn
lock that guards host worktrees does not cover this; the intent row is
the remote sessions' equivalent, and it is durable.

Spend and concurrency: the per-principal concurrent-sandbox cap is an
atomic reservation taken with the incarnation intent, not a
check-then-act count. Each session carries a cumulative spend ledger
with an owner-visible ceiling, because per-spawn ceilings multiply by
reincarnation. Ceilings are requested at spawn and enforced by the
confining environment; the ledger and cap are the machine's. A cap
refusal in-thread lists what is running with thread links and how to
stop one, not just a number.

The deployment operator sets those machine limits at boot. The defaults are
three live sandboxes per owner, 5,000,000 micro-USD per spawn, and 20,000,000
micro-USD per session. `TIDEBREAK_RUNTIME_CONCURRENCY_CAP` changes the first.
`TIDEBREAK_RUNTIME_SPAWN_SPEND_CEILING_MICROUSD` and
`TIDEBREAK_RUNTIME_SESSION_SPEND_CEILING_MICROUSD` change the spend limits;
`none` leaves that Tidebreak ceiling unset. The runtime profile may still
impose a lower ceiling. A refusal names the setting that the operator can
raise before restarting Tidebreak.

Two remote inputs stay refused until the runtime contract can preserve their
existing safety properties:

- Code trigger turns are at-most-once. Sandbox spawn and inbox calls accept no
  idempotency key and expose no replay result. If Tidebreak retried an
  ambiguous response, one pull-request event could run twice. Tidebreak keeps
  returning `remote_triggers_unsupported` until the runtime accepts a stable
  operation key and returns the prior outcome on replay.
- Image attachments never enter transcript text. The sandbox message contract
  carries text only, and spawn can clone repositories but cannot stage a
  bounded owner-scoped blob. Tidebreak keeps returning
  `remote_attachments_unsupported` until the runtime provides that file
  transfer. Base64 in the prompt and temporary repository commits are not
  acceptable substitutes.

Remote sessions get their own fence causes — incarnation intent
unresolved, sandbox lost mid-turn, terminal flush missing — because the
existing `FenceReason` variants describe local process supervision.
Reap is available to the owner in Slack as a button on the fenced
notice, and on desktop as today; a Slack-only user is never left with a
dead thread whose only exit is a surface they lack.

## Rendering

Render [`AttentionState`](decisions/0030-code-mode-separate-surface.md)
plus the per-turn assistant record — never the raw `AgentEvent`
firehose. Slack's agent APIs stream (`chat.startStream` /
`chat.appendStream` / `chat.stopStream`, threads only), show live
progress (Thinking Steps), and carry a working status
(`assistant.threads.setStatus` with rotating loading messages). The
supervised agent's event vocabulary widens in stage 1 to emit a bounded
per-turn assistant record, so there is something worth streaming; tool
activity and reasoning stay in the pod.

Per turn:

- A Thinking Steps timeline carries progress: lifecycle kinds map onto
  steps (clone, turn 2, WIP pushed) with no tool names and no
  arguments.
- The assistant record streams into the thread as the turn's answer via
  `chat.appendStream`, finalized on settle.
- `setStatus` covers the gaps between steps and is cleared explicitly on
  settle — the ~2-minute auto-clear is a timeout, not a contract.
- Where a surface lacks these (older clients, fallback paths), one
  status message per turn, edited in place, every edit carrying elapsed
  time ("Working — turn 2, 6m") so the line visibly breathes.

The first status in a session sets the expectation once: runs happen in
a sandbox and turns take minutes.

| Attention | Slack |
| --- | --- |
| `Working` | Live status with elapsed time and the latest lifecycle kind. No tool names. |
| `Stalled` | Warning with the idle duration the attention model already carries ("No activity for 90s"). |
| `NeedsYou` | Critical. Connect, or a failure prompt. |
| `DoneUnreviewed` | Success, then a separate artifact message. |
| `Idle` after an interrupt or a turn that ended without `DoneUnreviewed` | Terminal edit stating what happened ("Stopped by you", "Turn failed: …"). Never leave "Working" standing. |
| `Fenced` | Warning, the reason, an owner-only reap button. |
| `Manual` | Terminal edit noting the owner pinned the state; stop editing. |

The artifact is the pull request link, or — when the run pushed WIP
without opening one — a compare link
(`github.com/org/repo/compare/<base>...<ref>`) with a one-line summary
and a sentence saying what a WIP ref is. A bare ref name is never
posted; it is meaningless in Slack.

Disclosure is part of rendering. Artifacts land in the thread, readable
by every member, including guests and — in Slack Connect shared
channels — external organizations. In externally shared channels the
adapter degrades to link-only artifacts or DMs the owner. All adapter
posts set `unfurl_links: false, unfurl_media: false`, so a private
repository's metadata is not expanded into the channel by an unfurl app.
Administrators choose which repositories the shared instance may use through its
GitHub App installation. All admitted channels inherit that access; connecting a
workspace grants that shared authority.

Slack rate limits are budgeted per app per workspace, not per message:
the adapter runs a per-(workspace, method) token bucket and degrades by
dropping intermediate states, never terminal ones. The event watch holds
one WebSocket per session with an unsettled turn only, persists its
cursor in the render state, resynchronizes from the session's current
attention snapshot when replay reports truncation, and jitters
reconnects so an adapter restart is not a stampede.

Attachments, message edits, deletions, and thread-broadcast all have the
same v1 answer: ignored, and the status message says so the first time
an owner attaches a file ("Attachments aren't read yet").

## Commands and discoverability

The command surface is four words; nothing propagates by folklore:

- `/tidebreak help` lists everything with examples.
- `/tidebreak mode` names the permission mode a machine session should
  take, within the operator's ceiling.
- `/tidebreak identity` names who the next session acts as (`person` or
  `bot`), within the get-or-create rule above.
- The App Home shows grant status, active sessions with thread links,
  the channel defaults the user can see, and a revoke control.
- The install welcome DM introduces the mention pattern and the
  directive.
- `/tidebreak repo set owner/name` and `/tidebreak repo clear` manage a
  channel default. Anyone with a grant may set one — Slack offers no
  portable authority check, so the safety lives in the per-repository
  owner confirmation, the visible "set by @who" notice, and the App Home
  audit trail, not in a pretended admin gate.

## Desktop pickup

The session is an ordinary inbox item: owner, repository, coarse
journal, artifact. One provenance banner — "Started from Slack; engine
activity stays in the sandbox" — with a link to the thread, so the thin
journal reads as provenance rather than corruption. That banner is in
scope; further Slack UI is not.

The desktop may submit turns to the session — it is an ordinary session
on the owner's machine — and the adapter renders the resulting activity
with attribution ("a turn was started from the desktop"), so the thread
never shows status motion with no visible cause.

The pickup is honest about depth: the journal carries turns, the
per-turn assistant record, attention, and artifacts — not tool activity
or reasoning, which stay in the pod. The session view reads as a real
conversation with coarse interiors, and the banner says why.

## What to build

Schema changes are appended migrations
([`0061`](decisions/0061-schema-changes-are-migrations.md)). Stage 1 has
none.

### Stage 1: remote execution for repository sessions

The heaviest stage, and named as such: the provisioning client against
the pinned runtime contract (landed in `code/remote/`), incarnation
intent protocol, event
ingestion into the journal, WIP-ref resume with the new remote fence
causes, cap reservation, spend ledger, per-session idle stop. It also
widens the supervised agent's event vocabulary with a bounded per-turn
assistant record — the turn's answer text, size-capped — retained
durably in the journal. That record is what Slack streams, what the
desktop shows, and the substrate memory mines later; it ships here, not
as a rendering afterthought.

Verify: spawn a remote session with a repository; a repository outside
the GitHub App installation is refused with the rendered reason; a
session survives sandbox stop and resumes from WIP refs; a message
during a stop waits for the terminal events and then reincarnates
exactly once; two concurrent messages cannot double-provision; the
reconcile sweep cancels an intent that never activated; the cap refuses
the N+1th sandbox atomically; a killed sandbox fences with the remote
reason and reap recovers the workspace.

### Stage 2: external sessions and grants

The binding table (`owner, channel kind, external key → session`),
external get-or-create, the
messages endpoint (`new_turn`/`queued`, `ts` ordering, transactional
`event_id` idempotency), grant-tagged sessions, adapter tokens with
mint, rotate, reuse detection, list, and revoke, the connect approval
surface on hosted web, and the desktop grants list.

Verify: two racing creates yield one session; a replayed `event_id`
returns the row-derived outcome; out-of-order `ts` within the window
applies in order; a revoked grant's WebSocket drops immediately and the
next call fails; a grant cannot touch a session tagged to another grant;
the approval page shows the Slack identity and a forwarded link binds
nothing without the closing DM confirm.

### Stage 3: the adapter

Webhook verification and the accepted-event allowlist, the durable
inbound queue, the routing table with the pending-row protocol, render
state, connect handshake with the pending first message, the assistant
container (assistant-thread events, suggested prompts, thread titles),
streaming the assistant record with Thinking Steps progress,
per-repository confirmation buttons, terminal edits and fallback status
rendering, artifact rendering, disclosure rules, rate-limit budgeting,
`stop`, non-owner handling, `/tidebreak help`, App Home, welcome DM,
`/tidebreak repo`, reap button, instrumentation for the exit criteria.

Verify: an unmapped user's first message survives the connect round
trip and runs without retyping; a duplicate delivery produces no
duplicate turn; an adapter restart mid-turn resumes editing the same
status message and does not re-post the artifact; a machine that is down
past Slack's retry window still gets the message later and the thread
saw a failure notice meanwhile; a non-owner reply gets a reaction and
one notice and never reaches the engine; a `message_changed` event does
nothing; an externally shared channel gets link-only artifacts; the
fenced reap button works and is refused for non-owners.

### Stage 4: desktop pickup

The provenance banner, the thread link, and desktop-submitted-turn
attribution in the thread.

## Repository-less sessions

`POST /external/code/sessions` accepts a request that omits both `repo_id` and
`repository`. It creates a code session without a workspace or an initial clone.
An explicit request harness wins over the channel preference. When both are
unset, a Slack session uses the configured runtime's admitted default engine.
No runtime, or a legacy runtime without engine declarations, falls back to
Internal on the machine. An explicit Internal selection stays on the machine.
A configured but invalid default refuses admission; it does not fall back.
An explicit repository selector preserves the repository-backed path.

The same conversation can then choose repositories with `code_repos`, start
independent child sessions in new workspaces with `code_session_create` (each
with a stable
`request_key`), send follow-ups with `code_run_turn`, list children with
`code_sessions`, and poll for results with `code_wait`. Children inherit
the parent's owner, grant, and forge identity. Machine children keep the parent's
permission mode; configured sandbox children use Allow under sandbox confinement.
A revoked grant refuses discovery, creation, and child reads. Workspace grants
use the instance's GitHub App access across channels. The machine checks that
access before admitting a repository, including a cached checkout.

Decision [0094](decisions/0094-repository-optional-conversations-on-the-internal-engine.md)
keeps the current list of work that remains for self-drive child sessions.

## Explicit steering admission

The messages endpoint accepts optional `steer`, `expected_turn_id`, and
`correlation_uuid` fields. Existing clients keep queue-default behavior. To ask
for mid-turn delivery, set `steer: true` and name the active Tidebreak turn in
`expected_turn_id`. The server assigns a correlation UUID when you omit it.
The native turn number stays an internal sandbox detail.

To read a result without submitting again, call
`GET /external/code/sessions/{id}/messages/{event_id}/admission` with the adapter
token. The response returns `outcome` (`pending`, `steered`, or `queued`), the
original `turn_id`, `expected_turn_id`, `correlation_uuid`, and `reason`. An
unknown event or a session outside the grant returns 404.

Each channel `event_id` owns one durable admission. A retry returns the first
admission's target, correlation, and outcome. It never sends another steering
request. Reusing a correlation UUID for a different event fails.

An acknowledged native instruction returns `outcome: "steered"` with its
original turn and correlation. Proven unsupported or stale requests return
`outcome: "queued"` with `reason: "steer_unsupported"` or `"stale_turn"`; the
original message can run at a later turn boundary. Successful steering consumes
that original queue row atomically, so it cannot run a second time.

`reason: "unacknowledged"` means that delivery has not been confirmed. It does
not promise another execution. The admission remains held until native evidence
settles it. A sandbox inbox receipt proves only transport storage. A completed
write, timeout, idle sandbox, or restarted sandbox does not prove that the native
engine rejected the instruction. Clients must not resend with a new event ID or
show confirmed delivery from those signals.

Codex supplies native acknowledgments. Harnesses without acknowledged steering
fall back before dispatch. The sandbox must first advertise `steering_protocol: 1` in its
`supervisor_started` event with a fresh `runtime_id` UUID for each process;
older runtimes keep ordinary queue behavior. Sandbox acknowledgments must match
the stored sandbox, runtime UUID, native turn, Tidebreak turn, and correlation.
A replacement process cannot consume a frame addressed to its predecessor. A late local acknowledgment can
settle the original receipt while its worker still owns the turn.

To retire an unconfirmed admission, call
`POST /external/code/sessions/{id}/messages/{event_id}/recovery` with the bound
adapter token. Send `{"action":"discard"}` to remove its held queue row.
Discard does not undo work that the harness may already have performed. Send
`{"action":"retry","accept_duplicate_risk":true}` to queue another copy of the
original message. Require explicit acceptance that the original may already
have run before sending a retry request.

Recovery returns `action`, `recovered_at`, and `retry_turn_id` for a retry.
The admission GET also includes this object as `recovery`. Repeating the same
action returns the original decision and retry ID; choosing a different action
returns a conflict. Retry creates a fresh ordinary queue row. Recovery wakes
queue processing and preserves an explicit queue pause. An idle machine session
with no worker starts one so the recovered queue can run without another message.
If its prior worker still needs recovery, the endpoint reports that the decision
is saved but queued work cannot start.

Recovery remains separate from native admission. If native confirmation arrives
first, recovery refuses the request. If recovery commits first, late confirmation
can still settle the original receipt without deleting the retry. Clients can
retire recovery polling when `recovery` appears, even if `outcome` remains
`pending`. An ended session allows discard but refuses retry.

Keep automatic adapter steering disabled until the adapter exposes pending
admissions and their recovery controls. Native acceptance followed by process
loss can remain unknown; transport idempotency cannot settle that case.

## Later

- Automatic Slack steering, pending-admission recovery, and confirmed delivery
  rendering, gated on the reply-during-run signal.
- Slack Code channels as a session surface: one code channel per
  session, `AttentionState` feeding the native status.
- Durable child wait/resume, the broader sandbox-child tools, and the thread/web tree,
  as specified in the preceding section.
- Collaborator steer, and an owner relay affordance ("forward this to
  the session") as its cheaper predecessor.
- Local-desktop Slack via a relay.
- [`0048`](decisions/0048-one-interaction-model.md) step 5.

## Out of scope

- Slack as a connected-app catalog Tidebreak publishes (refused in
  [`deferred.md`](deferred.md); the adapter consumes Slack, it does not
  broker Slack for others).
- Driving repository-less threads through chat's separate surfaces rather
  than through the code-shaped session model. Repository-less threads are
  sessions on the internal engine, with their own workspace-less web
  link; the user-facing collapse of chat and code surfaces is deferred.
- Streaming tool activity or reasoning into Slack. The assistant record
  streams; the engine's interior does not.
- Reusing `tidebreak-sandbox-protocol`.
- Approving engine actions from a Slack button on a sandbox session. The
  repository confirmation and the reap button stay session-lifecycle
  consent. A machine session's approval, question, and plan cards are in
  scope (decision 91).
- Transcript-level resume for scratch sessions.

## Open questions

Recorded because reasonable people weigh them differently; the design
proceeds on the stated answers.

- More channels. GitHub issue and PR-comment driving would reuse the
  PR-fact substrate, triggers, and the GitHub App identity at low cost,
  and the channel-agnostic binding (see Design for leverage) is built so
  it can. Slack goes first because it is the surface people want;
  GitHub-comment driving is the natural second channel on the same
  endpoints, not a rejected alternative.
- The sandbox requirement. Mobile drives the shared hosted engine
  remotely today, and owner-text-only removes most channel-specific
  injection risk, so a hosted-engine interim path would delete the
  heaviest stage. The design keeps the sandbox as defense in depth for
  unattended `Allow` with an unscoped forge token, and because remote
  execution is wanted regardless — but if stage 1 stalls, this is the
  cut to consider, and it deserves a decision record either way.

## Related

- [`0032`](decisions/0032-code-workspaces-worktrees-checkpoints.md),
  [`0053`](decisions/0053-code-worktrees-live-in-a-user-visible-root.md) —
  workspace as folder.
- [`0039`](decisions/0039-allow-is-a-first-class-code-permission-mode.md) —
  Allow as the confined-sandbox posture.
- [`0047`](decisions/0047-gateway-linked-hosting.md),
  [`0072`](decisions/0072-mobile-client.md) — hosted machine, public
  client.
- [`0063`](decisions/0063-hosted-machines-borrow-forge-credentials.md),
  [`0065`](decisions/0065-hosted-git-acts-as-the-person.md) — GitHub App
  as git identity and allowlist; the unscoped user token.
- [`0073`](decisions/0073-agent-mcp-drives-chat-over-attach.md),
  [`0074`](decisions/0074-agent-mcp-drives-code-mode.md) — attach client.
- [`0079`](decisions/0079-supervised-agent-declines-the-sandbox-protocol.md)
  — the agent side of remote execution.
- [`deferred.md`](deferred.md) — remote session execution, unparked by
  stage 1.


### Several threads for one session

The adapter attaches a thread through
`POST /external/code/sessions/{id}/bindings` with `external_key`. The grant
must already hold the session. Repeating an attachment returns the existing
binding; a destination held by another session or grant returns not found.
Ended sessions return `409 ended`. `GET` on the same route lists the bindings.

A workspace grant also sends `channel_id` for conversation routing. The machine
checks the instance's configured GitHub App access to the session's repository;
the destination channel needs no separate repository approval. The adapter
checks workspace membership before calling; attachment does not change the
session's access rows. Each bound thread reads the same event stream with its
own cursor. Snapshots expose `external_origins` and preserve `external_origin`
as the first thread for older clients. Decision 93 records the boundary.

### Repository access across channels

Choose the repositories that the instance's GitHub App can access in the GitHub
App installation settings. Every Slack channel connected to that instance can
use those repositories. You do not enter repository names or approve them in
Tidebreak Channels.

The agent can discover and choose several repositories without a channel default.
A channel default only selects where work starts. To add or remove repository
access, change the GitHub App installation. The machine checks the configured
forge before starting repository work or attaching a task to another channel.

Historical channel approval records and endpoints remain compatible with older
clients, but do not determine repository access. Decision 96 supersedes the
per-channel approval policy in decisions 92, 93, and 94.

## Channel behavior settings

Tidebreak owns a Slack channel's harness, model, automatic replies, and added
instructions. Gateway owns execution permissions, credentials, sandbox resources,
and spending limits. A channel inherits the instance's GitHub App repository
access; channel behavior never creates another repository allowlist.

The adapter reads `GET /external/code/channels/{channel_id}/preferences` with
its live grant. Tidebreak keys the preferences by Slack workspace and channel,
so reconnecting or rotating a grant keeps the settings. An unset harness or
model uses the instance's existing selection behavior. Automatic replies default
to enabled for established Tidebreak threads; they do not start a conversation
from every channel message. A thread's quiet override remains separate.

The response supplies `settings_path`, which the adapter resolves against the
Tidebreak UI origin. The authenticated channel page uses the existing harness
and model catalogs. A human administrator can read or change shared settings.
A personal Slack connection does not prove membership in a private channel, so
it does not grant access to that channel's instructions.

A new session freezes its harness, model, and channel instructions. Changes to
those defaults apply to new sessions. Automatic replies remain live for existing
threads; a quiet thread resumes when an authorized person mentions the app.
Quiet leaves already accepted work running. Session creation returns the stored
`harness` and `model`; a null model means no model has been pinned, rather than
an inferred default presented as observed execution. The adapter should render
these values and link to Configure without adding another status message.
