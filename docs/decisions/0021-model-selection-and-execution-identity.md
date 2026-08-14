# 21. Separate Durable Model Selection from Frozen Execution Identity

- Status: Accepted
- Date: 2026-08-14
- Owners: model routing and turn execution
- Related: [`docs/model-providers.md`](../model-providers.md),
  [`0017-gateway-capability-negotiation.md`](0017-gateway-capability-negotiation.md),
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
policy-matched snapshot produces the same digests. At request time it obtains a
live route lease that serializes catalog replacement through one HTTP request
setup. Adapters repeat that authorization for every provider-managed
continuation request.
The host execution selector remains on the normalized request for native replay
gating, while a separate non-serialized wire-model field carries the
deployment-local id into the provider body. A third immutable request-shaping
identity carries the canonical upstream model family and version into provider
adapters. This lets a gateway-local alias retain provider-version behavior such
as Anthropic adaptive thinking, effort mapping, versioned web-search tools, and
signed reasoning replay without sending the canonical id on the wire or
weakening exact-route replay gating. The internal selector is never sent to the
gateway, and neither the local wire id nor the shaping identity replaces the
replay origin.

The execution record is produced from one cloned, policy-matched gateway
snapshot. Equivalence selection, capability derivation, validation, and the
persisted fingerprint all use that same snapshot value. No stage returns only a
local handle and asks another stage to reinterpret it from mutable state.

The worker resolves the persisted execution key only to recover the admitted
capability policy. The router is the final enforcement boundary: before sending
a gateway request, the route set must claim the exact frozen selector and its
live route authority must revalidate that selector against the current snapshot.
Each request-scoped lease is held until that HTTP leg is dispatched, so a sync
cannot replace the catalog between validation and dispatch. Because external
OS/MDM policy can change outside the process-local route lock, managed request
authorization validates the route, performs the potentially slow bearer mint,
then revalidates every policy, installation, snapshot, wire, canonical, and
shaping identity while retaining the same request lease through dispatch.
Anthropic `pause_turn` continuations repeat this sequence and mint a fresh
installation-pinned bearer before every leg. Managed policy must
therefore still point to the recorded deployment and the current policy-matched
catalog must contain the recorded local id with the same upstream identity,
protocol, and fingerprint. An unrelated catalog update may proceed; reuse or
mutation of the admitted route fails closed instead of retargeting the turn.
Credential loss also remains a normal provider-unavailable failure.

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

Client-supplied turn ids also have one durable global owner before mutable
admission begins. A `turn_admission` row binds each id to the owning chat and a
versioned fingerprint of the exact caller request: byte-exact content, ordered
image ids, ordered document ids, ordered invoked skills, and the voice-input
flag. Model selection, catalog state, skill availability, blob metadata,
document ownership, and capabilities are intentionally excluded because they
are mutable admission prerequisites rather than caller identity.

Admission ownership moves through `pending`, `queued`, and `accepted`. A
`pending` row carries a bounded lease token and expiry so another process can
wait for the first decision and recover ownership after a crash. Queueing and
turn acceptance consume the exact lease generation—id, token, and expiry—and
check expiry against the database's statement-time clock before transitioning
ownership in the same transaction that creates the queued row or accepted
turn. Queue promotion re-reads the unchanged FIFO head beneath the chat write
lock and atomically moves `queued` ownership to `accepted`, creates the message
and turn, and removes the queue row. Editing a queued message updates its
fingerprint under that same lock, while deleting it removes only queued
ownership. A stale promoter snapshot can therefore neither execute a retracted
or reordered message nor delete an edited one. The process-local per-turn mutex
remains a latency optimization. The database row is the cross-process
authority, so an exact retry can return the committed result without re-reading
mutable policy, catalog, skill, image, or document state, and a changed payload
or another chat fails with an identity conflict.

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

**Use only a process-local admission lock.** Rejected because desktop and
server processes can share the same database without sharing memory. A retry in
another process could revalidate mutable policy and fail even while the first
process was about to commit the exact request.

**Look up queued ids only inside the requested chat.** Rejected because a
client-supplied id is a global turn identity. A row queued in one chat could
otherwise be accepted live in another chat and later discarded during
promotion.

**Reserve every turn id permanently before validation.** Rejected because a
crash or ordinary validation failure would strand the id forever. Bounded
pending leases permit exact recovery while queued and accepted ownership remain
durable.

**Infer ownership by checking the queue and turn tables pairwise.** Rejected
because cross-table negative checks are awkward to serialize portably and
leave crash windows between the checks and writes. One admission row is the
authoritative state machine for the id.

## Consequences

Admission becomes the single owner of executable identity. The existing model
column remains sufficient because the frozen key is bounded by the column's
model-id limit. The new global turn-admission ownership table is a baseline
schema change and therefore advances the pre-1.0 desktop schema epoch. Workers
may recover display and capability data from the matching current row, but
cannot turn the frozen key into a different route. The router's
selector-to-wire-id-and-shaping-id rewrite becomes a security boundary and must
remain covered independently of higher-level tests.

A gateway administrator can make an accepted turn temporarily unrunnable by
removing or changing its exact route. That is preferable to silently retargeting
work. A retry may run once the identical route returns; a newly submitted turn
resolves against the new catalog.

The gateway catalog parser becomes stricter. A contradictory row loses curated
capabilities and cannot satisfy canonical equivalence until the gateway fixes
its provenance. Opaque custom gateway models remain usable when selected by
their explicit gateway key, provided their frozen route still matches.

Catalog sync and gateway HTTP request setup share a route-lease lock. A slow
catalog fetch may briefly delay a new gateway request, and request setup may
briefly delay snapshot commit; long-lived response streams do not serialize
other inference. No provider-managed continuation may dispatch without a fresh
route lease, bearer, and post-mint live-route validation.

Turn submission adds one short-lived admission row before mutable validation.
Concurrent exact retries may briefly wait for its lease holder, but once the
request is queued or accepted they return from durable identity alone. A
process crash delays takeover only until the bounded pending lease expires.

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
  record rather than falling back to mutable re-resolution;
- a gateway-local Anthropic alias uses its local id on the wire while canonical
  upstream identity preserves adaptive thinking, effort, versioned web search,
  and exact-route signed replay;
- an OS/MDM gateway repoint during bearer mint blocks the initial request and
  every provider-managed continuation before request data reaches the retired
  gateway;
- two store processes serialize the same exact turn id, recover an expired
  pending lease, and reject changed payloads or a different owning chat;
- queue creation, edit, deletion, and promotion preserve exactly one global
  owner and never leave the id simultaneously queued and accepted; and
- an exact retry of queued or accepted work succeeds without consulting mutable
  model, skill, blob, or document state.

A plausible wrong implementation can pass happy-path routing tests by storing
only `model_gateway::<local-id>` and resolving it immediately. Tests must pause
or replace the snapshot between admission and claim, and must reuse the same
local id for a different upstream identity, to prove the frozen boundary holds.
