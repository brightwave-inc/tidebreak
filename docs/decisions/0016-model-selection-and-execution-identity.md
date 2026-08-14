# 16. Separate Durable Model Selection from Frozen Execution Identity

- Status: Proposed
- Date: 2026-08-14
- Owners: model routing and turn execution
- Related: [`docs/model-providers.md`](../model-providers.md),
  [`0014-gateway-capability-negotiation.md`](0014-gateway-capability-negotiation.md),
  [`0002-pre-v1-schema-and-persisted-format-mutability.md`](0002-pre-v1-schema-and-persisted-format-mutability.md)

## Context

Tidebreak currently uses one model string for two different jobs. A chat or
global setting records what the user selected, while an accepted turn records
what the worker will execute. Those values happen to be interchangeable for a
direct provider model, but they are not interchangeable for a managed model
gateway.

A durable selection such as `anthropic::claude-opus-5` names user intent and is
portable across gateway deployments. A gateway route such as
`model_gateway::production` is only a deployment-local routing handle. The
catalog entry behind that handle supplies the upstream identity, protocol, and
capabilities that make it an executable route.

Resolving the portable selection on every read preserves intent, but it is not
safe after a turn has been accepted. A catalog replacement can reuse the same
local handle for a different upstream model between admission and worker claim.
Reading the snapshot once to choose a handle and again to construct its policy
can also combine two individually valid catalog revisions into a model the user
did not select. Repointing managed policy to another gateway creates the same
problem when two deployments use the same local handle.

The catalog may also carry several identity hints: the local id, an upstream
id, and aliases. The local id is a routing handle, not authoritative provenance
when upstream metadata exists. Accepting the first recognized hint allows a
contradictory catalog row to advertise one curated model while routing another.

Foreground admission and queued-turn promotion now resolve managed equivalents,
but on-demand compaction has an independent resolution path. That lets the same
chat use the gateway for ordinary turns and try the direct provider for
compaction, changing both routability and native reasoning replay provenance.

These are one ownership problem: Tidebreak has no shared contract for when model
intent is resolved, what becomes immutable, and which consumers must use it.

## Decision

Tidebreak will represent model choice in two stages.

**Durable selection is user intent.** Chat, global, sticky, and model-role
settings retain the canonical or explicitly selected key. Enabling a managed
gateway does not rewrite those settings to deployment-local ids. Reads may
resolve that intent against the current deployment for display or for admitting
new work, but they do not mutate it as a side effect.

**Executable identity is frozen when work is admitted.** Every accepted turn
persists a versioned execution key in the turn's existing model field, while
the durable selection remains on the chat or global setting. Direct routes keep
their ordinary provider-qualified key. A gateway execution key contains:

- the normalized managed gateway deployment URL;
- a digest binding the deployment-local model id;
- the gateway protocol;
- one unambiguous upstream identity, including its canonical curated selection
  when the catalog maps it to the registry; and
- stable digests of the deployment URL and the catalog fields that determine
  routing identity, protocol, and capabilities.

The gateway key is an internal, versioned selector rather than the model id sent
on the wire. The configured router claims that selector only while its current
policy-matched snapshot produces the same digests, then rewrites it to the
deployment-local id immediately before the provider adapter builds the request.
The internal selector is never sent to the gateway.

The execution record is produced from one cloned, policy-matched gateway
snapshot. Equivalence selection, capability derivation, validation, and the
persisted fingerprint all use that same snapshot value. No stage returns only a
local handle and asks another stage to reinterpret it from mutable state.

The worker resolves the persisted execution key only to recover the admitted
capability policy. The router is the final enforcement boundary: before sending
a gateway request, the route set must claim the exact frozen selector. Managed
policy must therefore still point to the recorded deployment and the current
policy-matched catalog must contain the recorded local id with the same
upstream identity, protocol, and fingerprint. An unrelated catalog update may
proceed; reuse or mutation of the admitted route fails closed instead of
retargeting the turn. Credential loss also remains a normal provider-unavailable
failure.

Non-registry test and development resolvers retain their free-form model
contract. Registry-enforced production admission always freezes a gateway route;
a plain gateway key remains a durable user selection or catalog key, not an
accepted execution identity.

Gateway identity derivation is fail-closed. When an upstream id or aliases are
present, every recognized authoritative candidate must resolve to the same
curated model. Conflicting recognized candidates produce no canonical
equivalence. The deployment-local id may be used as an identity hint only when
stronger provenance is absent, or when it agrees with all recognized stronger
hints. Unknown aliases do not manufacture identity; they remain opaque routing
metadata.

All work that executes in the context of a chat uses one shared executable-model
resolution seam. Foreground turn admission, queued promotion, on-demand
compaction, and future non-turn chat operations pass the same explicit-versus-
automatic selection semantics through that seam. Automatic fallback to the
first entitled gateway model remains allowed only when no explicit chat or
global selection exists.

Native provider reasoning and provider-tool replay remain exact-route artifacts.
Canonical equivalence is used to honor selection intent, not to translate or
re-sign provider-native blocks across routes.

## Alternatives Considered

**Persist gateway-local keys and resolve them at execution.** This is the
current shape and is compact. Rejected because local ids are namespaced only by
a mutable deployment, and a catalog can legally reuse one between admission and
claim. The persisted value looks immutable while its meaning is not.

**Resolve dynamically at every seam.** This keeps every operation current and
avoids a persisted execution shape. Rejected because separate snapshot reads
can observe different revisions, separate callers have already diverged, and an
accepted turn must not silently change models because administration changed
after acceptance.

**Persist only a catalog revision or ETag.** This detects any replacement but
invalidates admitted turns when an unrelated model changes. It also does not
state which identity fields Tidebreak trusts. Rejected in favor of binding the
specific route semantics; a revision may still be retained as diagnostic
provenance.

**Teach each provider pair how to translate native artifacts.** A gateway and
its upstream provider could be treated as the same model for reasoning replay.
Rejected because provider-native blocks are signed and route-coupled. Pairwise
translation scales poorly and violates the existing flatten-on-switch rule.

**Do nothing and ask users to retry after catalog changes.** Rejected because
the observed failure is not limited to a clean refusal: mutable resolution can
silently run a different model, and compaction can use a different provider
identity than the chat's turns.

## Consequences

Admission becomes the single owner of executable identity. The existing model
column remains sufficient because the frozen key is bounded by the column's
model-id limit; no schema or epoch change is needed. Workers may recover display
and capability data from the matching current row, but cannot turn the frozen
key into a different route. The router's selector-to-wire-id rewrite becomes a
security boundary and must remain covered independently of higher-level tests.

A gateway administrator can make an accepted turn temporarily unrunnable by
removing or changing its exact route. That is preferable to silently retargeting
work. A retry may run once the identical route returns; a newly submitted turn
resolves against the new catalog.

The gateway catalog parser becomes stricter. A contradictory row loses curated
capabilities and cannot satisfy canonical equivalence until the gateway fixes
its provenance. Opaque custom gateway models remain usable when selected by
their explicit gateway key, provided their frozen route still matches.

Revisit this decision if the gateway protocol itself provides a globally stable,
cryptographically bound deployment/model revision that can replace Tidebreak's
field fingerprint; if accepted turns gain a first-class server-side execution
lease that already freezes routing; or if post-1.0 schema compatibility changes
the cost of evolving the execution record.

## Validation

The implementation must cover these cases:

- a canonical Opus selection resolves from one snapshot and cannot become GPT
  when the snapshot changes between admission steps;
- an accepted gateway turn cannot run after another deployment or catalog row
  reuses its local id for a different upstream model;
- an unrelated catalog-row update does not invalidate the admitted route;
- conflicting local-id/upstream-id and conflicting recognized aliases produce
  no canonical equivalence;
- foreground admission and queued promotion persist the same execution shape;
- on-demand compaction uses the same gateway route as an ordinary turn for the
  same canonical selection;
- explicit unmatched or ambiguous selections fail closed, while a genuinely
  unset automatic selection may still choose the first entitled gateway row;
- same-route native reasoning and provider-tool replay survive, while a route
  change still drops or flattens those artifacts; and
- registry-enforced workers reject a missing or malformed frozen execution
  record rather than falling back to mutable re-resolution.

A plausible wrong implementation can pass happy-path routing tests by storing
only `model_gateway::<local-id>` and resolving it immediately. Tests must pause
or replace the snapshot between admission and claim, and must reuse the same
local id for a different upstream identity, to prove the frozen boundary holds.
