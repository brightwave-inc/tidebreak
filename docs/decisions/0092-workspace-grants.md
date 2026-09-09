# 92. Workspace grants

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

Under a workspace grant, external get-or-create requires a `channel_id`. An unconfirmed `(channel, repository)` pair is refused (`409 repository_unconfirmed`) and recorded as pending with the body's `set_by`. An administrator approves the channel’s repository scope in Settings > Channels. The scope can contain several explicit repositories and can be approved before the first task. Each approval persists for that channel and grant. Additional requests remain pending independently so a task can select several repositories. Person grants are unchanged.

The shared identity's forge credential is the ceiling. The channel’s approved repository scope is the gate. Approval never grants GitHub access that the shared identity lacks.

`POST /deployment/code/grants/workspace/{id}/channels/{channel_id}/repositories/approve` adds a batch of explicit repositories to that scope. The endpoint requires administrator authority, validates and canonicalizes the entire batch before writing, and commits all approvals together. Existing single-repository confirmations remain supported. Revoking the workspace grant removes its authority.

The scope flow replaces the original rule that a later request supersedes an earlier pending repository. Existing superseded requests can be approved explicitly or become pending again when retried.

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
- An administrator approves several repositories before a task starts; the channel can then use each repository without another confirmation.
- Requests for several repositories remain pending together; an invalid batch changes nothing.
- Another channel or grant cannot reuse the scope. Revoked and person grants cannot receive a channel scope.
- The actor from the body lands on the turn; a person grant refuses a body actor.
- Access rows are replaced by the adapter's list.
- Revoking the workspace grant fences every channel session at once.
