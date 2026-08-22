# 62. Per-caller gateway entitlements on hosted machines

- Status: Proposed
- Date: 2026-08-22
- Owners: server
- Related: [`0049-gateway-authenticated-hosted-machines.md`](0049-gateway-authenticated-hosted-machines.md),
  [`0051-on-behalf-of-inference-for-hosted-machines.md`](0051-on-behalf-of-inference-for-hosted-machines.md),
  [`../gateway-boundary.md`](../gateway-boundary.md)
- Supersedes: decision 51's presentation of per-caller inference as a curated
  Anthropic route, and its rule 3 — a stored provider statement no longer
  silences the per-caller path, because the two no longer compete for one
  provider. Decision 51's exchange, its attribution, and its fail-closed
  rules stand unchanged.

## Context

Decision 51 gave hosted machines per-caller inference but not per-caller
choice. The route it built posed as the curated Anthropic catalog: every
caller was offered the same hard-coded list, models their own gateway account
never entitled them to were offered and refused at the turn, and models the
deployment's gateway does serve — through either compat protocol — were
missing. Background roles resolved against stored provider rows a hosted
machine never has, so chat titling and the approval judge silently did
nothing.

The gateway now mints a second capability from the same machine-bound token
decision 51 exchanges: audience `catalog` returns a short-lived,
`models:read`-scoped token that reads exactly one thing, the caller's own
member catalog at `/api/v1/me/catalog` (model-gateway ADR 0082). That makes
the right question answerable per caller: which models does this person's
account entitle them to, right now.

## Decision

1. **The machine reads each caller's member catalog.** It exchanges the
   caller's live machine-bound token for a `catalog` capability and fetches
   `/api/v1/me/catalog`, converting the answer with the same code the managed
   sync uses, so a hosted caller's model rows are shaped exactly like a
   managed profile's. Snapshots live in process memory keyed by owner, beside
   the inference token they parallel: fresh for five minutes, revalidated
   with the held `ETag` after that, served up to an hour stale only across
   transport failure. A gateway refusal always propagates — a revoked
   session stops the caller rather than coasting on grace — and nothing is
   ever written to the store.

2. **A caller's routes are gateway routes.** Route collection builds the
   caller's routes from their snapshot with the same builder the managed
   profile uses: a route per compat protocol, raw and frozen model
   identities, the caller's rotating credential, and one inert adapter for a
   caller entitled to nothing. Nothing impersonates a direct provider
   anymore, which retires decision 51's rule 3 — a stored BYOK configuration
   keeps its own route beside the caller's gateway path, and neither
   displaces the other.

3. **Every catalog surface resolves per caller.** The picker, the providers
   list, selection validation, the turn-accept freeze, and the turn worker's
   re-resolution all take the requesting caller's snapshot. A hosted
   caller's explicit selection freezes against their own snapshot and
   resolves back through it and nobody else's. Without a resolvable
   snapshot, every one of these surfaces offers nothing: an unnamed request,
   a dead session, and a gateway outage past the grace all fail closed.

4. **Background work runs as the caller who triggered it.** Chat titling,
   workspace titling, and the approval judge resolve their utility model by
   walking the triggering chat's owner's entitlements, and drive it on that
   owner's credential. Work that can name no caller is skipped — never run
   as somebody else, never billed to the deployment.

5. **Named gaps, left open.** The web-search sub-request and the container
   sandbox's model proxy still resolve without a caller and therefore stay
   unavailable on hosted machines. Both need a caller-aware seam of their
   own; this record makes their absence explicit rather than accidental.

## Rejected

- **Reusing the deployment-wide snapshot.** One caller's entitlements served
  to everybody, written by a sync no hosted machine can run. Rejected before
  this record; recorded here because the store key still exists for managed
  profiles and must never be written per caller.
- **A client-supplied catalog.** The attached desktop could push its own
  catalog, but the server validates every selection, and background work has
  no attached client to borrow from.
- **Keeping the curated-Anthropic presentation with per-caller filtering.**
  Smaller diff, but it keeps offering one protocol, keeps the impersonation
  that forced rule 3, and forks gateway-model handling from the managed
  path that already does it right.

## Revisit when

- A hosted deployment needs the web-search sub-request or container
  sandboxes; either lifts a named gap from decision 5.
- The gateway serves per-caller MCP entitlements the same way; the apps half
  of the member catalog is already fetched and discarded here.
- Entitlement changes need to land faster than the five-minute freshness
  window; the revalidation cadence is a constant, not a contract.
