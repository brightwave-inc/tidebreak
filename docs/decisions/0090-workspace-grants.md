# 90. Workspace grants

- Status: Accepted
- Date: 2026-09-08
- Owners: thet
- Related: [0086](0086-session-access-is-separate-from-ownership.md); [0088](0088-a-slack-session-runs-where-the-deployment-can-run-it.md); [0089](0089-service-principals.md); `docs/slack-sessions.md`; tidebreak #3178 (track D), #3190
- Supersedes: none

## Context

Person adapter grants are minted through connect: the adapter parks a handshake, the owner approves on the hosted page, the adapter completes. A channel session has no particular person. It needs a grant owned by the service principal (decision 89) that an admin, not that service, can approve. The first use of a repository under a person grant is the owner's confirmation. A channel default that anyone can set is a routing attack if it can spend the shared identity's forge credential without an admin gate.

## Decision

Adapter grants have a kind, `person` or `workspace`. Existing rows are person grants.

A workspace grant is owned by a service-kind principal and covers one Slack workspace. A service principal starts the handshake (`POST /code/grants/workspace`). On a gateway-authenticated machine the machine enrolls the gateway delegation at start with the service's own lease, because that identity never has a browser. An admin approves (`POST /deployment/code/grants/workspace/{id}/approve`). The adapter completes as for a person handshake. The handshake records `approved_by` as the admin's owner id.

Under a workspace grant, external get-or-create requires a `channel_id`. An unconfirmed `(channel, repository)` pair is refused (`409 repository_unconfirmed`) and recorded as pending with the body's `set_by`. An admin confirms. A later different repository supersedes the pending row. Person grants are unchanged.

The shared identity's forge credential is the ceiling. The admin's confirmation per channel and repository is the gate.

Messages under a workspace grant take the actor from the body. `PUT /external/code/sessions/{id}/access` replaces that session's `external:<channel kind>:<user id>` contribute rows with the adapter's list. Revoking the workspace grant fences every channel session it tagged.

## Alternatives Considered

**Reuse person grants and have an admin click "is this you?" as the bot.** Rejected: that would treat the service as a person, which decision 89 forbids, and it would put a browser flow on an identity that never signs in.

**Confirm the repository once per workspace, not per channel.** Rejected: a default in one channel must not authorize another.

**Skip access rows and resolve membership only in the adapter.** Rejected: the web already reads session access rows (decision 86). Private-channel membership has to land there.

## Consequences

Workspace grants are visible to admins on the grants list, with the channels and repositories they cover. A hostile workspace admin can still act as any member of that Slack workspace; the gate is this machine's admin, not Slack's.

Revisit this if a workspace grant must cover more than one Slack workspace, or if the shared identity needs a forge credential other than the deployment's.

## Validation

- A service principal starts a workspace handshake; a person cannot.
- A non-admin cannot approve; an admin can, and the adapter completes.
- A channel session refuses until the repository is confirmed, then runs.
- The actor from the body lands on the turn; a person grant refuses a body actor.
- Access rows are replaced by the adapter's list.
- Revoking the workspace grant fences every channel session at once.
