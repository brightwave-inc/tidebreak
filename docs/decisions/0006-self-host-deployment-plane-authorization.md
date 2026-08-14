# 6. Self-Host Authorization: Admin and Member Roles on the Deployment Plane

- Status: Accepted
- Date: 2026-08-10
- Owners: server, security
- Related: [`crates/tidebreak-server/src/auth.rs`](../../crates/tidebreak-server/src/auth.rs)
  (token file and credential-to-principal resolution),
  [`crates/tidebreak-server/src/principal.rs`](../../crates/tidebreak-server/src/principal.rs)
  (`Principal`, `AuthContext`, the fail-closed extractor),
  [record 2](0002-pre-v1-schema-and-persisted-format-mutability.md)
  (pre-1.0 format mutability, which covers the token-file change below),
  issue #1460 (the decision this record settles)
- Supersedes: none

## Context

The self-host profile is meant to support a concrete deployment story: an
operator takes the server artifact, runs it inside their own
network — their VM, their VPC, their database — and hands tokens to a small
team. The operator owns the infrastructure and the secrets; OpenWave never
sees the deployment. That story is the near-term goal, not a hypothetical.

**What is true today.** Authentication and data scoping are built (#853):
every self-host credential resolves to a named `Principal::User`, and data
rows — chats, projects, documents, transcripts, the event stream — are
owner-scoped through `ScopedStore`, on which cross-owner queries do not
exist. But the configuration surface is gated only by `require_token`, which
any configured user passes. That surface includes:

- `PUT /mcp/servers`, which spawns operator-specified `command`+`args`
  processes **on the host** (`crates/openwave-server/src/mcp_config.rs`);
- provider, web-search, and code-execution credentials (write, delete, and
  presence reads);
- model role assignments, global settings, plugin install/enable, and
  connected-app sign-in/sign-out.

Net effect: on a shared deployment, every token-holder is an administrator of
the deployment — arbitrary command execution on the host and custody of the
shared secrets. Data scoping contains what a member can *read*; nothing
contains what a member can *reconfigure*.

**What is assumed.** Teams are small and low-ceremony: one or two people
administer the deployment, everyone else uses it. Nobody has asked for
delegated partial administration, per-user provider credentials, or
organizational sub-groups, and this record does not design for them.

## Decision

Split the API into two planes, and make the split structural rather than
per-handler.

**1. Every route belongs to one of two planes.** The **member plane** is
owner-scoped data and read-only capability discovery: chats, projects,
documents, turns, approvals, the event stream, and reads that tell a client
what the deployment can do (model list, plugin catalog, app library,
non-secret config reads). The **deployment plane** is everything that changes
what the deployment *is* or touches its shared secrets: MCP server
configuration, provider/web-search/code-execution credentials (including
presence reads that reveal secret metadata), model roles, global settings
writes, plugin install/enable, and connected-app sign-in/sign-out.

**2. Principals carry a role: `Admin` or `Member`.** The desktop profile's
`LocalOwner` is unconditionally admin — one person at the machine, nothing
changes. On self-host the role comes from the token file: a line gains an
optional third field, `admin` (`<user-id> <token> admin`); absent means
member. A user's role must be consistent across all of their token lines, and
a self-host boot with zero admins fails at token-file load — a deployment
nobody can configure must not start, for the same reason an empty token file
must not.

**3. Enforcement is a router property, not a handler habit.** Deployment-plane
routes are assembled into their own sub-router carrying a `require_admin`
layer over the already-attached `AuthContext` — fail closed, like the
existing extractor: no context or no admin role is a `403`. Handlers do not
individually check roles. The point is where mistakes land: a new config
route added to the deployment-plane router is admin-gated by construction,
and a new route added to the wrong router is caught by the conformance suite
below, not by a reviewer's memory.

**4. The identity seam is unchanged.** The token file remains the
credential-to-principal resolver, and the role rides the same seam. When an
external identity provider later replaces the file (#578), it must answer the
same question — *which user, which role* — behind the same middleware; the
plane split does not care where the answer comes from.

**5. Deployment posture, stated so docs and hardening agree.** The supported
shape is the server plus PostgreSQL inside the operator's own network, with
TLS termination and network exposure owned by the operator's fronting
infrastructure. Two riders land with the role work because they are cheap
and this record is where the posture is written down: `MIN_TOKEN_LEN` rises
from 16 to 32 (tokens are operator-generated; the module docs already
recommend 64-character values), and the self-host docs must note that the
WebSocket token travels in `Sec-WebSocket-Protocol`, which intermediary
proxies log more readily than `Authorization` headers.

**Deliberately excluded:** roles beyond admin/member, teams or groups,
per-capability grants, per-user provider credentials, and any UI for role
management (the token file is the management surface). Packaging the server
for one-command deployment (container image, compose file, infra templates)
is follow-up work this record enables but does not specify.

## Alternatives Considered

**Declare self-host single-tenant** — document that all tokens are equivalent
and every token-holder is the operator. Cheapest option, and honest about
today's code. Rejected because it contradicts the goal: a deployment you hand
to a team is exactly the case where "every token is root on the host" is
unacceptable. A loud doc warning does not make a footgun safe; it makes it
documented.

**Defer self-host as a supported posture until after launch.** Rejected for
the same reason — the team deployment is the near-term target, and deferring
the authorization model just moves this same decision later while the config
surface's shape ossifies.

**Full RBAC: fine-grained roles, per-capability grants, groups.** Rejected as
premature. The concrete gap is binary — host command execution and shared
secret custody versus everything else — and two roles close it. A grant
vocabulary designed now, before any real demand for delegated partial
administration, would be guessed rather than derived, and persisted guesses
are the expensive kind (see record 2 for why we keep pre-1.0 formats few and
deliberate).

**Per-user configuration isolation** — each member brings their own provider
keys and MCP servers. Rejected: the configuration surface is deployment-shared
*by design* (shared credentials are the point of a team deployment, and MCP
servers are host processes, which makes per-user servers a sandboxing
problem, not an authorization problem). Nothing here forecloses it later; it
would slot in as member-plane routes writing owner-scoped config rows.

**Trusted-header authentication from a fronting proxy** — let the operator's
reverse proxy or load balancer authenticate (say, against their identity
provider) and pass the resolved identity in a request header the server
trusts. Rejected as the default: the scheme is only as strong as the
guarantee that every request actually traversed the proxy, and the
characteristic misdeployment — the server port reachable directly — fails
open, where a bearer token fails closed. It remains a legitimate *future*
authenticator behind the same credential-to-principal seam, carrying user
and role the way the token file does today; adopting it would be a
configuration option for operators who already run such a proxy, not a
redesign.

**Do nothing.** Rejected — that is the current state, and it is the finding
in #1460.

## Consequences

- **The token-file format changes.** Existing two-field files remain valid
  lines, but a file with no `admin` entry now refuses to boot a self-host
  server. That is a behavior break for any existing deployment; record 2's
  pre-1.0 mutability covers it, and the boot error says exactly what to add.
- **Members lose abilities they never should have had.** Clients used against
  a member token get `403` on settings-write surfaces. The desktop UI is
  primarily driven by the desktop profile (always admin), so the immediate
  cost falls on API/CLI use; making the desktop settings panels degrade
  gracefully against a member token is follow-up UI work, not a blocker for
  the role gate itself.
- **Every future config route pays a classification decision.** That is the
  intended cost: the author must say which plane a route belongs to, in the
  router, where review can see it.
- **The plane split constrains #578.** A future identity provider must supply
  a role, not just a user id, or default everyone to member.

Revisit this decision if: someone needs delegated partial administration
(that is the RBAC trigger, and the grant vocabulary should be derived from
that request, not invented earlier); per-user provider credentials become a
real ask (the per-user-config alternative above); or external identity (#578)
lands with a group model that makes file-managed roles redundant.

## Validation

- **A route-table conformance test, not spot checks.** Drive one admin and
  one member principal across the assembled router and assert, for every
  deployment-plane route, member → `403` and admin → non-`403`; and for a
  representative member-plane set, member → non-`403`. Enumerating the actual
  router is the point — a wrong implementation that gates by matching path
  prefixes in middleware would survive spot checks on the routes someone
  remembered to list, and would silently exempt the next config route whose
  path doesn't match the pattern.
- **Boot behavior:** a token file with tokens but no admin fails to load with
  an actionable error; a user whose lines disagree about role fails parse;
  the desktop profile's behavior is byte-for-byte unchanged (LocalOwner
  passes every admin gate).
- **The plausible-wrong-implementation case:** a `require_admin` layer that
  checks the role but attaches its own default `AuthContext` when none is
  present would pass every authenticated-request test. The conformance suite
  must include a request that reaches an admin route with no auth middleware
  in the stack and assert it is rejected, mirroring the existing fail-closed
  extractor test.
