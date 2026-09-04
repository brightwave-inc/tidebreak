# 86. Session access is separate from ownership

- Status: Accepted
- Date: 2026-09-04
- Owners: thet
- Related: [0047](0047-gateway-linked-hosting.md) (amends its validation rule that a second user never sees another owner's events); [0049](0049-gateway-authenticated-hosted-machines.md); [0082](0082-the-hosted-machine-serves-the-renderer.md); `docs/slack-sessions.md`; tidebreak #3178 (track A), #3179, #3180
- Supersedes: none

## Context

`ScopedCode` binds every `/code/*` read and write to the caller's owner key.
Another owner's session is indistinguishable from one that does not exist,
the updates channel fans out to the owner alone, and a turn records what was
said but not who said it. That is the right floor for a machine one person
uses. It is the wrong shape for what Slack asks of a machine: a thread in a
channel is read by everyone in the room, several of them steer it, and a
link in the thread should open the same session on the web for a teammate
who never connected. Nothing in the model lets a session be seen by a
second person, let alone driven by one, and nothing names who drove it.

Two facts about the code are load-bearing. First, ownership is more than
access: the owner's key is the execution identity (whose forge credential
and inference the session spends) and the lifecycle authority (who may
reap, delete, or change the permission mode). Second, the adapter already
holds one external identity per person (`code_external_grant`), which is
what the machine knows about a Slack user who has not signed in here.

## Decision

A session keeps exactly one owner, who remains its execution identity and
lifecycle authority. Access is a separate, per-session list, and visibility
is a per-session default.

- `session_access (session_id, subject, level, granted_by, created_at)`,
  `level` in `view` or `contribute`. A subject is a principal key
  (`principal:<owner key>`) or an external identity
  (`external:<channel_kind>:<user id>`), so the Slack adapter can mirror a
  private channel's membership without the machine knowing those people.
  An external-identity row resolves for a web caller only through a live
  external grant that binds them to that identity; a revoked grant makes
  the row inert.
- `session.visibility` is `private` (the default) or `deployment`. A
  `deployment` session may be read by any authenticated principal on the
  machine. Visibility never grants writes.
- Reads (the session, its workspace, turns, events, attention, artifacts,
  approvals) resolve for the owner, any access row, or `deployment`
  visibility. Submit, queue, steer, interrupt, and approval decisions need
  `contribute` or ownership. Reap, permission mode, model, settings,
  delete, and access management stay with the owner, or an admin where the
  route already admits one.
- The updates channel and the session event socket fan out to every
  granted reader and sever a reader's stream the moment their row is
  revoked or their grant is fenced.
- Every turn and every approval, question, or plan decision records an
  actor: the principal when there is one, the external identity and
  display name when the input came through an adapter, the trigger when a
  trigger fired it. Older rows stay null and render as the owner.

A channel session (track D) does not enumerate its readers here: its
contributor set is the channel's membership, resolved by the adapter at
each turn under the shared identity that owns the session. This record
covers DM sessions, web sharing, and private-channel membership the adapter
mirrors into rows.

Excluded: teams and roles (they wait on team ids from the gateway's
principal read), transfer of ownership, and any access that outlives the
session.

## Alternatives Considered

**Ownership as a set.** Several owners, each an execution identity. Rejected:
the forge credential and the inference bill would have no single principal,
and a reap by one owner would surprise another. One owner, many readers, a
few contributors keeps every spend and every destructive action attributable
to one person.

**Visibility alone, no rows.** `private` or `deployment`, nothing per
person. Rejected: a DM session shared with one teammate would have to become
visible to the whole deployment, and a private channel's session could not
be limited to the channel's members.

**Rows keyed only by principal.** Rejected: a private channel's members are
Slack identities, most of whom hold no principal on the machine until they
connect, so the adapter could not mirror membership.

**Do nothing.** Sessions stay single-reader; Slack threads would keep
carrying a transcript the web cannot show and steering only the owner may
do. Rejected; it is the reason for track A.

## Consequences

Two appended migrations (decision 61). Every read path in `ScopedCode`
gains a second resolution step, so the owner-scope test suite grows a
viewer, a contributor, a revoked reader, and a `deployment` reader, on both
the desktop and the self-host profile. Decision 47's validation rule
narrows from "a second user never sees another owner's events" to "a second
user sees another owner's events only through an access row or `deployment`
visibility, and never writes without `contribute`".

The actor makes attribution a stored fact rather than an inference, which
the adapter and the web both render. It also means a display name reaches
the machine's database; it is the name the channel showed, nothing more.

Revisit when the gateway's principal read carries team ids (rows keyed by
team) or when a session needs to change owner.

## Validation

- A viewer reads the session, its turns, and its events, and every write
  is refused; a contributor submits and steers and cannot reap or change
  the permission mode.
- Revoking a row closes that reader's open event socket within one event.
- A `deployment` session is readable by a third principal that holds no
  row; a `private` one answers not found to the same principal.
- An external-identity row resolves through a live grant and stops
  resolving when the grant is fenced, without the row changing.
- A wrong implementation that passes the old suite and still fails this
  one: resolving access by the workspace's owner instead of the session's,
  or fanning out to viewers on the updates channel but not on the event
  socket.
