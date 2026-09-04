# Slack sessions

Status: proposed. This page is the working design. It is not a decision
record. Where a later record disagrees, the record wins. This revision
follows an adversarial review pass; the largest changes are recorded in
"Bets and exit criteria" and "Open questions".

A person talks to Tidebreak in Slack — in the agent's own chat (Slack's
primary and split view for AI agents) or in a channel thread. Tidebreak
runs a session for that conversation on that person's hosted machine.
The session's engine runs in a confined sandbox. Progress streams back
into the conversation through Slack's agent surfaces. The same session
appears in the desktop inbox.

Version one ships both surfaces: the agent DM and channel threads.
Getting the agent into a conversation uses Slack's own affordances —
"Add agent", the channel's Agents & apps tab, @mention — and none of it
is custom chrome. Version one requires a repository. Repository-less
scratch sessions are a designed later stage: they carry the only schema
migration in this design, so they wait for evidence (see "Bets and exit
criteria" and "Stage: scratch workspaces").

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
- A code workspace assumes one repository throughout: `repo_id`,
  `worktree_path`, `branch_name`, and `base_ref` are all `NOT NULL`,
  branch names derive from the repository's branch prefix, and roughly
  eight `get_repo(workspace.repo_id)` sites in the runtime plus the PR
  delivery sweep, triggers, orphan reclaim, and the generated wire types
  depend on it. Making the repository optional is a cross-cutting
  refactor, not a one-column migration. That is why scratch is a later
  stage.
- `tidebreak-supervised-agent` clones each declared repository in order
  and runs in the first clone. WIP refs are
  `mg-wip/<sandbox-id>-i<incarnation>` for the first clone,
  `-r<position>`-suffixed for later ones. An empty repository list is a
  research run: no clone, no WIP push, and the deliverable leaves as a
  `task_output` event body (224 KiB cap) emitted only at supervisor stop —
  not per turn. The scratch stage depends on changing that.
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

State the compromise honestly: a compromised adapter can submit prompts
to existing sessions of every connected user and read those sessions'
events. It cannot pick repositories freely, because session creation in a
repository requires per-repository owner confirmation (see Choosing the
repository) — the confirmation state lives on the machine, keyed by
grant, so the adapter cannot forge it. That is the bound, and it is only
as good as that confirmation gate.

## Identity and connect

A Slack user does not run until they hold a grant.

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

Only the session owner's messages reach the session. Anyone else's reply
gets an acknowledging reaction from the bot and, once per user per
thread, an ephemeral notice that says what they can do: who the owner
is, that the agent does not read the thread, and how to connect
themselves. Non-owner text never reaches the engine, even as context —
a channel is an injection surface and the engine runs with the owner's
unscoped forge token. Collaborator steer is a later grant.

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

The session uses the code-shaped journal and attention model, not a chat
row. A repository-less ask through chat stays refused: chat is the
internal engine on the machine, and Slack wants a confined sandbox.

| Slack | Tidebreak |
| --- | --- |
| First @mention in a thread | Get-or-create the external session; submit the message as the first turn |
| Later owner reply | Submit to the messages endpoint; outcome is `new_turn` or `queued` (see Ingestion) |
| Owner reply `stop` | Interrupt. The status message gets a terminal edit: "Stopped by you" |
| Reply while `Fenced` | Owner sees the reason and an owner-only reap button; anyone else sees the fenced notice |
| Reply while `Ended` | Refuse, with the context-correct next step: "send `new`" in a DM, "start a new thread and mention me" in a channel |
| Non-owner reply | Reaction plus one ephemeral notice; dropped |
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

A conflict response of `ended`, `fenced`, or unknown-session is the
defined signal for the adapter to durably close its routing row and
render the refusal. Get-or-create returns `ended` rather than
resurrecting. On a get-or-create hit, the pinned repository wins; a
conflicting spec in the retry is reported, never applied.

## Choosing the repository

Stage 1 requires a repository. Resolution at first contact, then pinned
on the workspace:

1. A `repo:owner/name` directive in the first message. A near-miss —
   wrong spacing, an inaccessible or misspelled name — refuses loudly
   before anything is created ("Did you mean `repo:owner/name`?"), never
   falls through.
2. Else the channel default. A DM has no default.
3. Else refuse, with the directive syntax and a pointer to
   `/tidebreak help`. Nothing spawns.

Bare GitHub URLs in prose are context, never clone intent.

The first use of any repository under a grant requires owner
confirmation: an owner-only button ("Run in `org/name`? Set by @who as
this channel's default"), verified by the interaction payload's user id,
recorded machine-side against the grant. This is the gate that makes the
channel default safe — Slack exposes no reliable channel-authority
concept, so in practice any member can set a default, and without the
gate a default is a routing attack: point it at a readable repository
whose contents prompt-inject an Allow engine holding the victim's
unscoped token. With the gate, a changed default cannot silently
redirect anyone. Setting or changing a default also posts a visible
notice naming who changed it. This confirmation is session-lifecycle
consent, not an engine approval; the ban on approving engine actions
from Slack stands.

The GitHub App installation intersected with the person's access is the
allowlist, refused with a human-readable rendering: outside the App
installation → "ask an admin to add it to the Tidebreak GitHub App";
no personal access → "you don't have access to `org/name`". No second
Slack-only allowlist until someone needs stricter than the App.

Changing the repository means a new thread. The first status message
carries the recovery inline: "Wrong repo? Reply `stop`, then start a new
thread with `repo:owner/name`."

## Execution

The session's engine runs in a per-session confined sandbox driven by
`tidebreak-supervised-agent`
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
worktree root. The location is stored on the session, reported on the
snapshot and the external event stream, and never changes. The machine
engine is the floor, not an interim path: a deployment without a
gateway, and a gateway deployment without a configured runtime, runs
Slack sessions the way it already runs desktop, mobile, and `agent-mcp`
sessions. An earlier version of this page refused to ship Slack on the
machine engine; the refusal rested on the sandbox being the only thing
that made unattended `Allow` safe, and the answer is not to withhold
sessions but to withhold `Allow`.

Permission mode follows the location. Inside a sandbox the engine is
`Allow`; confinement is the permission boundary
([`0039`](decisions/0039-allow-is-a-first-class-code-permission-mode.md)).
On the machine the session takes the deployment's default mode, and the
owner answers approvals from the desktop or the web, where the session
is visible like any other, until the channel can carry them. Slack
renders no harness approval cards yet. Slack `NeedsYou` is connect,
fenced, or failed — never `approval_requested`.

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
The per-repository confirmation is where "you are about to run a private
repository in a public channel" gets said.

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

## Stage: scratch workspaces (deferred, designed)

Repository-less sessions return when usage asks for them. What they
cost, recorded so the gate is honest:

- The optional-repository refactor: `repo_id`, `worktree_path`,
  `branch_name`, `base_ref` become nullable (a table rebuild in SQLite,
  in both dialects' fixtures), a partial unique index, and a defined
  NULL behavior for every consumer — release, restore, retry, the PR
  delivery sweep, triggers, orphan reclaim (which must not sweep
  remote/scratch rows), and the wire types the desktop consumes.
- Supervised-agent changes beyond stage 1's assistant record: emit the
  `task_output` body at every successful turn boundary, not only at
  supervisor stop — otherwise a multi-turn scratch thread has no
  per-turn artifact and loses unemitted output when an incarnation dies.
- Resume is journal-primed and says so in product copy: a preamble
  built from prior turn inputs and retained outputs under an explicit
  byte budget that states what it dropped. An incarnation killed before
  flushing loses its unemitted output; the preamble names the gap.
  Transcript-level resume for scratch is out of scope, permanently.
- A decision record: scratch amends 0030's "context selects behavior"
  sentence (no repository no longer selects chat) and changes what
  [`0048`](decisions/0048-one-interaction-model.md) step 5 must merge.
  That belongs in `docs/decisions/`, not only here.
- Expectation copy: a scratch session is a task runner, not a chatbot;
  the first status says so, because every Slack bot the user knows
  answers in seconds and this one provisions a sandbox.

## Later

- Steer as an upgrade to the messages endpoint (`steered` as a third
  outcome), capability-gated per harness, honest rendering ("queued
  behind the running turn" where mid-turn steer is unsupported) — gated
  on the reply-during-run signal.
- Slack Code channels as a session surface: one code channel per
  session, `AttentionState` feeding the native status.
- Scratch workspaces, as specified in the preceding section.
- Collaborator steer, and an owner relay affordance ("forward this to
  the session") as its cheaper predecessor.
- A Slack-only allowlist narrower than the GitHub App.
- Local-desktop Slack via a relay.
- [`0048`](decisions/0048-one-interaction-model.md) step 5.

## Out of scope

- Slack as a connected-app catalog Tidebreak publishes (refused in
  [`deferred.md`](deferred.md); the adapter consumes Slack, it does not
  broker Slack for others).
- Driving repository-less threads as chat sessions.
- Streaming tool activity or reasoning into Slack. The assistant record
  streams; the engine's interior does not.
- Reusing `tidebreak-sandbox-protocol`.
- Approving engine actions from a Slack button. The repository
  confirmation and the reap button are session-lifecycle consent, and
  that line is the boundary.
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
