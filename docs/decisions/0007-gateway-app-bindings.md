# 7. Gateway Connected-App Bindings for Local Apps

- Status: Proposed
- Date: 2026-08-10
- Owners: core, desktop
- Related: [`docs/local-apps.md`](../local-apps.md) (the manifest, grant, and
  invoke machinery this extends), [`docs/connected-apps.md`](../connected-apps.md)
  (the binding vocabulary and the managed-profile refusal),
  [`docs/gateway-boundary.md`](../gateway-boundary.md) (policy, session, and
  token audiences), record 2 (pre-v1 persisted-format mutability)
- Supersedes: none

## Context

A local app's manifest binds two kinds of capability: declared REST operations
of locally-configured `rest_api` connected apps, and connected folders. On a
gateway-managed profile the `rest_api` kind is refused entirely — local
credential entry is exactly what the managed lockdown exists to close — so a
managed user can author only apps with no network surface at all. The users a
gateway serves are the ones with the most to gain from app-shaped views of
their org's systems, and they are the ones who cannot build them.

Meanwhile the model gateway is growing a shared-apps surface (its ADR 0036)
whose authoring model is **draft-first**: a harness registers a draft app at
the gateway, pins gateway connected-app operations in its manifest as
`{connected_app_id, operation_ids[]}`, and drives the same
session-authenticated invoke route the app's eventual viewers will use —
execute-as-viewer from the first call, with the author as the only reachable
viewer. That design deliberately rejects a harness-side translator that would
convert local bindings at share time. Promotion is only ever mechanical if the
local binding vocabulary already names what the gateway's manifest names, so
the vocabulary is what has to be settled first.

**What is true today.** The gateway lists a user's entitled apps at
`/api/v1/cli/apps` with identity only — id, name, kind, enabled, endpoint
slugs — and no operation catalogs. OpenWave holds a PKCE session with
per-audience tokens, `control` being the audience for `/api/v1/cli/*`. Gateway
apps surface in OpenWave today only as `gateway_endpoint` MCP servers, and app
manifests cannot bind MCP tools at all — that vocabulary is retired (#1332,
#1589). Gateway-attested endpoints refuse app-driven session-token calls by
design, because a local-app invoke carries no model-emitted observation.

**What is assumed**, and is in flight on the gateway side: an additive per-app
catalog read on the CLI resource tier (operation ids, methods, display
summaries, no upstream URLs or credential material), and the draft-app
registration and invoke routes of ADR 0036.

## Decision

**A third `AppBinding` arm**: `{ gateway_app, operation_ids[] }`, where
`gateway_app` is the gateway's connected-app id — the same identifier the
gateway's own shared-app manifests bind. The existing enum discipline carries
over unchanged: the arms are untagged and discriminated by distinct field
names, unknown fields are refused, and a manifest carries at most one binding
per gateway app.

**Dispatch is a relay, not an executor.** A gateway binding executes by
relaying to the gateway's shared-app invoke route as the signed-in user — the
draft's author-viewer — with a `control`-audience session token. Deliberately
not attestation: these are human-initiated UI calls, where the session is the
principal and the draft manifest plays the role an attested observation plays
on the model path. That is ADR 0036's posture, mirrored on this side rather
than re-argued. OpenWave enforces its own refusal ladder first — pin, then
grant, then fingerprint currency — and the gateway re-enforces entitlement,
manifest pin, and credential resolution live on every call. No credential for a
gateway app ever exists locally; the gateway's typed `authorization_required`
failure crosses to the app frame machine-readably, so the app can render a
connect prompt rather than an error.

**Fingerprint.** Grants pin a canonical form beside the `rest_api` and folder
forms:

```json
{"v":2,
 "kind":"gateway_app",
 "gateway_base_url":string,
 "gateway_app_id":string,
 "catalog_sha256":string}
```

The base URL means re-pairing to a different gateway makes every gateway grant
stale; the catalog hash means a re-ingested app re-prompts. Entitlement is
deliberately **not** fingerprinted: it is the gateway's live predicate,
re-evaluated per call, and losing it fails the call rather than revoking the
consent. Fingerprints stay computed from definition fields and never from
storage, as every other kind's is.

**Consent posture.** A gateway binding is a network binding and is presented as
one. The consent sheet lists it by the app's display name, its opaque gateway
app id, and its pinned operation ids only — no gateway URLs, the same leak
posture the `rest_api` rows already hold — and it participates in the
combined-exfiltration warning exactly as a local operations binding does: a
manifest carrying a folder row and a gateway row can read files and send data
out, and the sheet must say so.

Deliberately excluded: local caching of gateway catalogs (they are read live,
like every other fingerprint input); projecting gateway operations into chat
turns as tools (a separate consent system, already recorded as not-taken in
[`connected-apps.md`](../connected-apps.md)); the draft-registration lifecycle,
which is its own slice built against this vocabulary; and any second invoke
transport — MCP mounts stay chat-only.

## Alternatives Considered

**Translate at publish only.** Author locally against `rest_api` records and
convert the bindings when sharing. Rejected on both halves: managed profiles
have no local `rest_api` surface at all, so the users the gateway serves could
never author the apps worth sharing, and an author who never ran the app the
way viewers will has not tested the thing being published. This is the
harness-side half of the promotion translator ADR 0036 rejects; draft-first
makes authoring and viewing one mechanism instead of two that must agree.

**Reuse the MCP mount surface.** Gateway apps already reach OpenWave as
`gateway_endpoint` MCP servers, so a manifest could pin their tools. Rejected
three times over: app manifests cannot bind MCP tools at all since the
retirement, gateway-attested endpoints refuse session-token app calls by
design, and a tool list discovered live from an upstream has no stable document
to hash — so a pinned consent has nothing to be current against. That last
point is the same catalog-currency argument ADR 0036 makes against MCP
bindings, and it lands identically here.

**Mirror gateway apps into local `rest_api` records** — synthesize local
records whose base URL is the gateway. Rejected: it fabricates exactly the
credentialed local record class managed profiles exist to refuse, the local
executor's egress and credential model does not apply (the gateway injects the
viewer's credential, not the record's), and record lifecycle would shadow live
entitlement with stale local rows. The binding must name the gateway's app
directly, or it is lying about who executes.

**Do nothing.** Managed users keep zero app-network surface and promotion stays
hypothetical. Rejected because the gateway is building the other half now: the
vocabulary has to be fixed before roster, consent, and dispatch code spread a
different one.

## Consequences

- `AppBinding` and `AppGrantBinding` become three-armed. Stored manifests and
  grants are persisted formats under record 2's pre-v1 rules, so the change is
  a baseline edit plus an epoch bump — no dual-reading layer.
- This is the first app-invoke path that leaves the machine, and on a managed
  profile it is the only one. An invoke may cost two gateway round-trips: the
  fingerprint-currency read and the relay itself. Accepted — the gateway is the
  org's own deployment, and every fingerprint input is already read live per
  request.
- The dispatch seam and the draft-id seam constrain the later registration
  slice to slotting a resolver in, rather than reshaping invoke around it.
- Revisit if: the gateway ships a catalog-document hash on the CLI read, in
  which case `catalog_sha256` should tighten to it; the gateway grows a
  non-draft direct invoke tier reachable to harnesses, which collapses the
  draft-id seam; per-operation approval policies land in gateway manifests, at
  which point grants may need per-operation rows rather than an operation list;
  or gateway MCP tool catalogs become hash-pinned, at which point a fourth arm
  could join this same shape.

## Validation

- **Vocabulary.** Closed-parse tests for the third arm: mixed-shape bindings
  refuse, and a pre-existing grant never reads as a gateway grant.
- **Fingerprint.** The digest derives from exactly base URL, app id, and
  catalog hash — moving any of the three moves it, and nothing else does.
- **Leak pin.** The grant projection for a gateway binding carries display
  names and operation ids only, asserted on the raw serialized JSON the way the
  `rest_api` pin is.
- **Ladder.** Unpinned refuses as `not_pinned`; ungranted or stale refuses as
  `consent_required`; no session or an unregistered draft produces a typed,
  teachable refusal; and a gateway `authorization_required` crosses to the
  frame machine-readably rather than as prose.
- The case a plausible wrong implementation still passes: relaying before the
  grant gate, or treating entitlement loss as fingerprint staleness, both leave
  a naive happy-path invoke test green. The ladder tests above are what
  distinguish them — the first by refusing an ungranted call before any network
  call is made, the second by asserting that a revoked entitlement fails the
  call while leaving the grant intact.
