# 47. Gateway-linked hosting: machines, clients, and roster-derived identity

- Status: Proposed
- Date: 2026-08-19
- Owners: server, desktop
- Related: [`0006-self-host-deployment-plane-authorization.md`](0006-self-host-deployment-plane-authorization.md),
  [`0017-gateway-capability-negotiation.md`](0017-gateway-capability-negotiation.md),
  [`0030-code-mode-separate-surface.md`](0030-code-mode-separate-surface.md),
  [`docs/gateway-boundary.md`](../gateway-boundary.md),
  [`docs/self-hosting.md`](../self-hosting.md),
  [`docs/deferred.md`](../deferred.md),
  [`0048-one-interaction-model.md`](0048-one-interaction-model.md),
  [`0049-gateway-authenticated-hosted-machines.md`](0049-gateway-authenticated-hosted-machines.md)
- Supersedes: none

## Context

The self-host profile exists so a team can run one shared Tidebreak server:
PostgreSQL store, a principal-naming authenticator, fail-closed boot, and plain
HTTP behind the operator's fronting infrastructure (decision 6). At the time
of this decision, the only implemented authenticator was an operator token
file and the product had no way to attach its own client to that deployment.
Decision 49 completes the Gateway-authenticator and desktop-attachment parts
of this plan; this record remains the machine/client and owner-scoping record.

Meanwhile the desktop app is deliberately local. The server refuses
non-loopback hosts in the desktop profile, the client has no product path for
a user-supplied remote URL and token, and host authority — folder broker,
client executor, native export, computer use — rides credentials the renderer
never holds, which `docs/host-access.md` already names as "the intended
consequence for host authority and a defect for anything else."

What was proposed here: model gateway deployments (the product
[`docs/gateway-boundary.md`](../gateway-boundary.md) already integrates with
as an OAuth client) are growing the ability to deploy `tidebreak-server` as
an optional component of the gateway installation — colocated database, with
fronting and TLS from the operator's infrastructure and identity derived from
Gateway users. Decision 49 deliberately skips the provisional generated roster
and verifies Gateway-issued user credentials directly.

Two facts bound the design:

- **Code mode is not multi-user-safe today.** The `code_*` tables carry no
  `owner` column and the `/code/*` routes sit outside the owner-scoped
  regime that decision 6 built for chat, documents, and apps. A shared
  deployment cannot enable code mode until that closes.
- **Tidebreak is pre-1.0 and keeps its breaking-change freedom.** The
  baseline migration currently calls self-host databases durable. A
  gateway-linked deployment must not quietly acquire a durability promise
  that slows the product down.

Assumed, not yet true: the end-state product shape is a **machine** — a
place agents run, whether the loopback server inside the desktop app or a
hosted server — and **clients** that attach to machines. Decision 48 defines
the single interaction model those clients speak.

## Decision

1. **A gateway-linked deployment is a self-host deployment whose identity
   derives from Gateway accounts.** Decision 49 specifies the implemented
   mechanism: Tidebreak verifies a dedicated resource token through Gateway,
   receives the stable user UUID and live administrator bit, and keeps static
   token files only for standalone compatibility. The generated-roster bridge
   described during this record's design is not the hosted default.
2. **Machine and client become the product vocabulary.** A running
   `tidebreak-server` instance is a machine. Renderer-shaped clients — the
   desktop webview, and later web and mobile surfaces — attach to a machine
   over the existing HTTP and WebSocket wire. Capability negotiation
   (decision 17) governs client-to-machine version skew the same way it
   governs the gateway boundary: probe, don't pin.
3. **The desktop app gains a remote connection mode** — the deferred member
   client, chosen over a hosted web UI as the first client because its
   approval and consent surfaces already exist. Gateway-backed connection
   takes a base URL and reuses the desktop's existing Gateway OAuth session;
   standalone compatibility takes a base URL and static token. TLS is required
   unless the host is loopback, mirroring the pairing rule in
   [`docs/gateway-boundary.md`](../gateway-boundary.md).
   While attached to a remote machine, host-authority features degrade
   legibly: routes and tools that require the client executor or the host
   broker are absent or refused with stable reasons, on the pattern headless
   write-back already set (`output_writeback_authority_unavailable`). A web
   or mobile client is the same shape with less authority, and follows once
   the remote wire is proven.
4. **Code mode on a shared machine is gated on owner scoping.** The `code_*`
   tables gain owners, and `/code/*` joins the owner-scoped regime, before a
   multi-user deployment enables code mode. This is the first work item, and
   decision 48 sequences it as convergence step one.
5. **Gateway-linked deployments are disposable until 1.0.** For this
   deployment class, the durable-self-host posture is narrowed: a baseline
   edit may drop and recreate the shared store, as the desktop schema epoch
   does locally. The deployment documentation states it, and operators of a
   gateway-linked deployment are told the store carries no retention
   promise. Ordinary operator-managed self-host deployments keep the
   existing back-up-before-upgrade posture; the narrowing applies only where
   the gateway tooling owns the database and can recreate it.

Deliberately excluded: synchronizing a desktop profile's local data with a
hosted machine (a distributed-state problem this record does not open);
serving the gateway's own traffic or learning its schema (the boundary in
[`docs/gateway-boundary.md`](../gateway-boundary.md) is unchanged in the
client direction); remote session execution in managed sandboxes (the
reverse-RPC spike's territory, its own future record); and host authority on
remote machines — a hosted machine has no trusted host executor, and this
record does not claim one.

## Alternatives Considered

- **A hosted web UI as the first client.** Serves the subway case directly,
  but every consent, approval, and degradation surface would be built twice
  before the wire is proven once. The desktop connection mode exercises the
  same routes with surfaces that exist. Web follows; rejected as first.
- **Keep the token file as the permanent identity story.** Two rosters per
  team, drift, and hand-managed revocation. Rejected; the file is the
  bridge, not the destination.
- **Trusted-header auth from the fronting proxy.** Decision 6 already
  rejected it as a default (fails open when the port is reachable directly)
  while naming it a legitimate future authenticator. Nothing here changes
  that judgment; the gateway authenticator will be a bearer-shaped
  credential, fail-closed.
- **Wait for 1.0 before hosting.** Postpones exactly the usage that will
  teach us what the 1.0 compatibility surface should be. The disposable
  regime in decision point 5 exists so hosting does not have to wait.
- **Do nothing.** The deferred items stay parked and every team's answer to
  "can I steer my agents from my phone" stays no.

## Consequences

- The renderer must treat host authority as conditional everywhere it
  invokes shell commands today; each callsite needs a remote-attached
  behavior, and "silently do it on the server's host instead" is the defect
  the validation section targets.
- The desktop profile's loopback lockdown is untouched; a new connection
  mode must not weaken the origin and loopback refusals for the local
  server.
- Decision 49's Gateway authenticator makes deactivation, session revocation,
  and role changes effective on the next live principal validation. The
  next-provision window applies only to standalone static-token operation.
- The disposable regime means a team's hosted history can vanish on upgrade.
  That is accepted on purpose, stated in operator documentation, and revisited
  at 1.0, when the compatibility commitment inverts the pre-1.0 rules.
- Owner scoping for code mode is now on the critical path of two records
  (this one and decision 48), which is the point: it is paid once.
- Revisit when a web or mobile client starts (the client surface may deserve
  its own record), or at 1.0.

## Validation

- Existing refusals hold: a desktop per-launch token is refused on a
  self-host deployment; a non-loopback host is refused in the desktop
  profile. New refusal: the remote connection mode refuses a non-TLS,
  non-loopback URL.
- The plausible wrong implementation of decision point 3 half-works: a
  remote-attached client that routes a native export or folder operation to
  the *server's* filesystem. The test attaches remotely and asserts the
  stable refusal, not a success against the wrong host.
- The plausible wrong implementation of decision point 4 scopes reads but
  not events: a second user on a shared machine must see neither another
  owner's `code_*` rows nor their session events on the updates channel.
- Gateway-identity drill: inactive, revoked, and wrong-resource credentials
  are refused; the server still fails closed when neither Gateway identity nor
  a standalone token file is configured.
