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

## Finding the OpenAPI document

The REST setup form does not assume you already have the document URL.
Enter the API's https base URL and use **Find the OpenAPI document**. The
server probes a fixed list of well-known paths on that origin (`/openapi.json`,
`/swagger.json`, `/v3/api-docs`, and similar), with the same https, SSRF,
size, and time bounds as a document fetch. Choose a candidate to fill the
document URL and list operations.

If nothing turns up:

1. Check the vendor's developer portal for an OpenAPI, Swagger, or API
   reference download link, and paste that URL.
2. If they publish no document, author a minimal OpenAPI 3 JSON listing
   only the operations you need (one GET with a path parameter is enough
   to ingest) and paste it.
3. If they offer an MCP server instead, add it under Settings → MCP
   servers.

YAML, Swagger 2.0, and HTML documentation pages are reported as found but
unusable; convert to OpenAPI 3 JSON. Discovery does not search the web or
ask a model.

The vocabulary matches the model gateway's (`connected_apps` with an
`app_kind` including both REST and MCP kinds) deliberately: a managed
profile's gateway-served apps and an unmanaged profile's local ones read as
the same concept, and promotion (below) is a translation, not a reframing.
The known semantic stretch — a filesystem MCP server is not an "app" in any
product sense — is accepted for that symmetry, as the gateway accepted it.

## The executor

The `rest_api` kind requires machinery Tidebreak does not have today: a
host-side executor that performs a declared operation against a connected
app on behalf of a caller. Its guarantees, all server-side and fail-closed:

- The request is validated against the ingested operation catalog — path
  template, method, parameter shape. Undeclared operations do not execute.
- The credential is injected by the executor at request time. It is never
  serialized to the renderer, a frame, a model prompt, or a log.
- Egress is bounded: DNS resolution pinned per request, private-network
  destinations refused, redirects refused, request and
  response byte counts capped, per-request timeout. Loopback HTTP is
  the single exception, below.
- Response bounds are sized for interactive UIs, not model context
  windows — deliberately larger than tool-result clamps.

This is the local form of the gateway's governed REST-tool contract, without
multi-user governance — which is exactly why managed profiles do not get it
(below).

## Local services on this computer

Plain HTTP is admitted only for a tightly validated loopback destination,
never for a remote or private-network host.

- The host must be an **IP literal** in `127.0.0.0/8` or exactly `::1`.
  `localhost` and every other DNS name are refused for `http` (the message
  tells the operator to use `127.0.0.1` or `[::1]`), so hostname resolution
  cannot widen the exemption.
- The record must carry `allow_loopback_http: true`. That flag is persisted
  on the `rest_api` definition and is part of the consent fingerprint, so
  toggling it re-prompts. Without the flag, admission names
  `allow_loopback_http` in the refusal.
- The exemption applies only to that record's own admitted origin (scheme,
  host, and port). Every executed request is pinned to it. Redirects are
  never followed — the same `reqwest` `Policy::none()` used for https — so
  a 302 to another host or another loopback port is reported, not chased.
- The OpenAPI document URL may be loopback HTTP under the same flag;
  otherwise it still requires https. Loopback-http spec fetches do not
  follow redirects.
- Remote `http` (including `10.0.0.0/8` and other private ranges) stays
  refused with the existing "scheme must be https" message. https keeps
  today's denied-network list, including loopback https.
- The Settings form shows a consent checkbox when the typed base URL is
  loopback HTTP and keeps Save disabled until it is checked. A path ending
  in `/mcp` is treated as an MCP HTTP endpoint and pointed at Settings →
  MCP servers (remote HTTP server) rather than this REST form.

## Bindings key off the app

A local app's manifest binds capabilities of connected apps. The vocabulary
is app-first, and since the tool-binding retirement (#1332) it has exactly
one live kind:

- `{ app, operation_ids[] }` for `rest_api` apps.
- `{ app, tools[] }` for `mcp_server` apps existed as the founding
  vocabulary — the only one available when local apps shipped — and is
  retired: `create_app` refuses to author it, consent conflicts instead of
  granting it, and the invoke route refuses the tool surface even under a
  pre-retirement grant. Stored manifests still parse and read as
  ungrantable. MCP was app-bindable only because it predated REST bindings;
  once REST landed there was no remaining app-shaped use for a vocabulary
  whose consentable universe included local stdio processes.

Grants, the consent sheet, the invoke route's refusal ladder, and live
fingerprint invalidation carry over unchanged; the `rest_api` fingerprint is
SHA-256 over base URL + OpenAPI document hash + credential *reference* +
placement + `allow_loopback_http`. Rotating the credential value behind the
same reference does not invalidate consent; repointing the reference or
toggling loopback HTTP does.

A manifest may also bind `{ gateway_app, operation_ids[] }` — an app the
model gateway holds, named by the gateway's own id (record 10). It is not a
connected-app record and nothing about it resolves locally, so it keys off
its own namespace and carries its own canonical fingerprint form: the gateway
origin, the app id, and a hash of the operation catalog the gateway declared.
Both the authoring roster and that fingerprint are read live from the
signed-in session — no gateway catalog is ever cached here — so a profile
with no session can neither author nor grant one.

The consent sheet leads with the app's display name ("Sentry") and lists
pinned capabilities under it — a legibility improvement over leading with
mounted tool names.

## Migration: hard cut, grants dropped, re-consent expected

Absorbing MCP definitions into the connected-app record is a forward
migration of storage, and it is deliberately a **hard cut**. Local apps
shipped days before this design and have, to a near certainty, no granted
users; the migration spends that fact rather than preserving what almost
nobody holds:

- The absorption **drops any pre-existing app grants** instead of
  translating them. An affected app simply re-presents its consent sheet
  on next open — the design's failure direction is "re-ask", never
  "widen", so the worst case is one click per app per user, and this is
  also the honest choice: the epic changes what a grant *names* (an app
  identity rather than a raw server namespace), and consent given under
  the old vocabulary should be re-asked under the new one, not silently
  reinterpreted.
- No legacy `{ server, tools[] }` vocabulary survives the migration —
  manifests and grants are app-keyed from the first slice, with no
  dual-reading compatibility layer anywhere in runtime code.
- Fingerprints remain **computed from definition fields, never from
  storage** — that is what makes a fingerprint identify the thing the
  user agreed to — but the canonical form is free to change (a `v:2`
  form keyed to the new record shape is expected). No hash-stability
  fixture across the migration is required.

Settings phasing: the MCP servers page remains during the epic; the
end state is one Connected apps page listing both kinds (per-kind detail —
health and mounting for MCP, catalog and credential status for REST). The
phasing is presentation only; the record is authoritative from its first
slice. The absorption has since completed: the standalone MCP servers page
retired into the Connected apps page (its editor unchanged, `/settings/mcp`
redirecting there), with no change to the record.

## Consumers

Exactly one in v1: **local-app bindings.** A connected app's catalog is not
projected to the model as tools. That convergence — Tidebreak projecting
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
  server-owned connector scaffolding; a future kind, not part of this epic.
- Filesystem bindings and their consent posture — #1331, designed in
  [folder-bindings.md](folder-bindings.md): folders bind as first-class
  roots, deliberately not a connected-app kind.
- ~~Narrowing which MCP transports are app-bindable — #1332, decided once
  REST bindings exist.~~ Decided and done: rather than narrowing by
  transport, MCP tool bindings were retired entirely (see "Bindings key off
  the app" above).
