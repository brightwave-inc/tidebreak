# Connected apps

The umbrella for outside integrations a profile can reach: one record class,
one Settings surface, one consent vocabulary, with per-kind mechanics behind
it. Today's MCP servers become one *kind* of connected app; this page's main
job is to specify the second kind — a plain REST API with a credential — and
the governed executor it requires. Local apps ([local-apps.md](local-apps.md))
are the first consumer; the design deliberately leaves room for others.

This is the design contract for the REST-bindings epic (#1330). It records
decisions; the implementation slices are filed against it.

## The record

A **connected app** is a profile-scoped record: opaque id, display name,
`kind`, and a kind-specific definition. Users think "I connected Sentry" —
the record is that thought made durable, whatever the transport underneath.

Two kinds initially:

- **`mcp_server`** — today's MCP server definitions, absorbed. The
  definition is the existing transport config (stdio command/args/env
  selection, HTTP url, gateway endpoint). Nothing about connection
  management, discovery bounds, health, or mounting changes; see
  [mcp-servers.md](mcp-servers.md).
- **`rest_api`** — a base URL, an OpenAPI document (bounded, ingested once
  to an operation catalog keyed by `operationId`, mirroring the validation
  posture the model gateway applies to operator-supplied specs), a
  credential *reference* into the profile secret store, and a placement
  (`Authorization: Bearer` or a named header). The credential value never
  leaves the secret store except inside the executor at request time.
  Credential-less entries (public APIs) are allowed and still governed.

The vocabulary matches the model gateway's (`connected_apps` with an
`app_kind` including both REST and MCP kinds) deliberately: a managed
profile's gateway-served apps and an unmanaged profile's local ones read as
the same concept, and promotion (below) is a translation, not a reframing.
The known semantic stretch — a filesystem MCP server is not an "app" in any
product sense — is accepted for that symmetry, as the gateway accepted it.

## The executor

The `rest_api` kind requires machinery OpenWave does not have today: a
host-side executor that performs a declared operation against a connected
app on behalf of a caller. Its guarantees, all server-side and fail-closed:

- The request is validated against the ingested operation catalog — path
  template, method, parameter shape. Undeclared operations do not execute.
- The credential is injected by the executor at request time. It is never
  serialized to the renderer, a frame, a model prompt, or a log.
- Egress is bounded: DNS resolution pinned per request, private-network
  and loopback destinations refused, redirects refused, request and
  response byte counts capped, per-request timeout.
- Response bounds are sized for interactive UIs, not model context
  windows — deliberately larger than tool-result clamps.

This is a scoped local analog of the gateway's governed REST tools
(its ADR 0008), minus multi-user governance — which is exactly why managed
profiles do not get it (below).

## Bindings key off the app

A local app's manifest binds capabilities of connected apps. The end-state
vocabulary is app-first:

- `{ app, tools[] }` for `mcp_server` apps — today's `{ server, tools[] }`
  is the legacy spelling of this, keyed by namespace instead of record id.
- `{ app, operation_ids[] }` for `rest_api` apps.

Grants, the consent sheet, the invoke route's refusal ladder, and live
fingerprint invalidation carry over unchanged; only the pinned vocabulary
and the per-kind fingerprint differ:

- `mcp_server`: the existing canonical definition form (`v:1`, fields not
  storage — see below).
- `rest_api`: SHA-256 over base URL + OpenAPI document hash + credential
  *reference* + placement. Rotating the credential value behind the same
  reference does not invalidate consent; repointing the reference does.

The consent sheet leads with the app's display name ("Sentry") and lists
pinned capabilities under it — a legibility improvement over leading with
mounted tool names.

## Migration invariant: fingerprints survive the absorption

Absorbing MCP definitions into the connected-app record is a forward
migration of storage, and one invariant is named here so no slice
improvises it: **the `v:1` server-definition fingerprint is computed from
definition fields, never from where or how the definition is stored.** A
migration that preserves fields preserves every existing app grant. If it
is ever violated, the failure mode is benign by construction — grants go
stale and users re-consent — but wholesale re-consent churn is a bug, not
an acceptable cost.

Settings phasing: the MCP servers page remains during the epic; the
end state is one Connected apps page listing both kinds (per-kind detail —
health and mounting for MCP, catalog and credential status for REST). The
phasing is presentation only; the record is authoritative from its first
slice.

## Consumers

Exactly one in v1: **local-app bindings.** A connected app's catalog is not
projected to the model as tools. That convergence — OpenWave projecting
REST catalogs into chat turns the way the gateway does — may be wanted
later, but it is a separate consent system (the chat approval gate, not app
grants) and a separate decision, recorded here as deliberately not taken
rather than drifted into.

## Managed profiles

On a gateway-managed profile the `rest_api` kind is refused entirely: local
credential entry is what the managed lockdown exists to close, and the
gateway is the sole governed REST channel — the same posture as BYOK
provider keys and manual MCP servers. Promotion of a local app with
`rest_api` bindings translates each binding to a gateway connected-app
operation set one-to-one; the local record's catalog vocabulary was chosen
so this is mechanical.

## Non-goals here

- OAuth-brokered kinds (Drive/Box-style source connectors) — the reserved
  `openwave-connectors` scaffold; a future kind, not part of this epic.
- Filesystem bindings and their consent posture — #1331.
- Narrowing which MCP transports are app-bindable — #1332, decided once
  REST bindings exist.
