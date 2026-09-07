# 89. Service principals own sessions without signing in

- Status: Accepted
- Date: 2026-09-07
- Owners: thet
- Related: [0006](0006-self-host-deployment-plane-authorization.md); [0049](0049-gateway-authenticated-hosted-machines.md); [0087](0087-standalone-browser-sign-in.md); [0088](0088-a-slack-session-runs-where-the-deployment-can-run-it.md); tidebreak #3178 (track D), #3189
- Supersedes: none

## Context

A channel session needs a durable owner even when no particular person owns the
work. On a standalone machine, the token file names every principal. On a
gateway-authenticated machine, the gateway principal read names the caller.
Treating this shared identity as a person would let its credential enter a
browser session and would hide why its sessions use deployment credentials.

Owner-scoped storage and worktree paths already use `user:<id>`. Changing that
key would split existing queries and paths without adding useful isolation.

## Decision

A service principal is a member. It owns sessions, data, and worktrees through
the same `user:<id>` key as a person, but its principal and new sessions carry
a service kind so surfaces can label the owner. Existing session rows use a
null kind, which means person.

A service principal never signs in through a browser flow and never becomes an
administrator. Standalone token files name one with `name token service`; they
reject a line that combines `service` and `admin`, and only a person marked
`admin` satisfies the boot check. Gateway principal reads may return `kind` and
`username`; an absent kind means person for compatibility.

A service principal executes forge operations as the deployment. On a
standalone machine, use the configured `GH_TOKEN` or GitHub App installation.
Do not add a per-service forge sign-in or credential store.

## Alternatives Considered

**Use an administrator account.** Rejected: channel work must not carry
deployment configuration authority, and a shared credential must not sign in
as a person.

**Create a separate owner namespace.** Rejected: the service needs ordinary
member ownership, while changing the owner key would disturb every scoped query
and worktree path.

**Give each service its own forge credential.** Rejected: the service represents
the deployment and should execute as the deployment credential already used on
that machine.

## Consequences

Sessions gain nullable owner metadata, so old rows continue to mean person and
new service-owned rows can be labeled without a data rewrite. Token and gateway
authentication share one principal kind, while browser sign-in keeps an
explicit refusal.

A service credential remains powerful enough to create and run member-owned
work. Operators must protect it like any long-lived API credential. Revisit
this decision if services need authority narrower than member ownership or a
forge identity different from the deployment.

## Validation

- Parse a token file with one person administrator and one service member.
- Refuse an admin-service line and a file whose only principal is a service.
- Return 403 when a service credential reaches either browser sign-in path.
- Create a session as a service and return `owner_kind: "service"` in its snapshot.
- Preserve `user:<id>` owner keys for people and services.
