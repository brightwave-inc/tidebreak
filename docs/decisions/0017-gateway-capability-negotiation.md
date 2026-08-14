# 17. Gateway capability negotiation, not version pinning

- Status: Accepted
- Date: 2026-08-13
- Owners: gateway connector / providers
- Related: 0010 (gateway app bindings), the model gateway's own member-catalog
  contract (`GET /api/v1/me/catalog`, `surfaces`, denial codes) shipped in
  model-gateway #635/#636

## Context

One Tidebreak install pairs with many gateways: a work deployment, a
customer's, a laptop compose stack. They upgrade on their own schedules, and
Tidebreak ships from its own channel — there is no moment where both sides
step together, and the `openwave` → `tidebreak` client-name migration already
demonstrated how expensive an ordered deploy is to coordinate.

The gateway now serves a member catalog (`GET /api/v1/me/catalog`: entitled
models and apps in one envelope, with per-app readiness and an `ETag`),
advertises its surfaces on `/api/v1/meta` and `/api/v1/cli/me`
(`{ member_catalog: "v1", denial_codes: [...] }`), and stamps designed denial
codes on refused inference (`x-model-gateway-error`, e.g.
`model_not_granted`). Older gateways serve none of that.

## Decision

Tidebreak negotiates capabilities per gateway and never pins versions.

- **Probe, don't compare versions.** The catalog is fetched directly; a 404
  means the deployment predates it and the sync degrades to the
  `/api/v1/cli/models` + `/api/v1/cli/apps` reads. `gateway_version` is for
  diagnostics only; no code path branches on it.
- **New Tidebreak, old gateway: degrade.** Everything the old wire cannot
  express (app readiness, instant catalog updates, designed denial copy) is
  hidden or generic, never an error. The settings panel may *note* an older
  gateway; nothing blocks.
- **Old Tidebreak, new gateway: additive wire only.** Unknown catalog fields,
  surfaces keys, and denial codes are ignored (no `deny_unknown_fields` on
  gateway wire types; unknown denial codes fall through to generic
  classification; unknown app readiness renders as generic not-ready copy).
- **Neither side waits on the other to ship.** A breaking change gets a new
  path or surface name, never a silent reinterpretation of an existing field.

The synced snapshot records which contract fed it (`member_catalog`,
`catalog_etag`), so the picker's provenance survives restarts and the next
sync can be conditional.

## Alternatives Considered

- **Pin a version matrix** (client ↔ gateway semver ranges): rots the moment
  either side back-ports, and turns a customer's stale gateway into a client
  that refuses to open. Rejected.
- **Ship the client from the gateway image** so versions always match, as the
  gateway does for `modelctl`: desktop signing/notarization and a second
  updater fighting the existing channel cost more than a tolerant protocol.
  Rejected.
- **Do nothing** (keep scraping `/api/v1/cli/models` only): loses per-app
  readiness, merged protocols, alias-based curated matching, cheap
  conditional refresh, and designed denial copy — the picker then can't say
  *why* a model vanished. Rejected.

## Consequences

Every new gateway surface lands here twice: the preferred path and the
degraded one, with tests for both. Wire types for gateway payloads must stay
open (additive, unknown-tolerant), which forgoes strict-parse error catching
on that boundary. Revisit if gateway deployments ever become centrally
version-managed (a fleet where the matrix cannot rot), or if a surface
arrives that genuinely cannot degrade — either would justify a hard minimum
gateway version at pairing time instead.

## Validation

- Catalog sync tests cover both the preferred member-catalog response and the
  legacy models/apps fallback after a 404.
- Gateway wire fixtures accept unknown additive fields and unknown denial
  codes without failing the whole response.
- No client behavior branches on `gateway_version`; it remains diagnostic
  metadata only.
