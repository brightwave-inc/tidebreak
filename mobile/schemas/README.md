# Vendored schemas

## `gateway-admin-openapi.json`

The model-gateway **control-plane (admin v1)** OpenAPI document, served by the
gateway at `GET /api/v1/openapi.json`. It is generated from the gateway's own
Rust handlers (utoipa), so it describes the shape of the API, not the contents
of any installation: no server URLs, no installation state, no credentials.
The gateway serves the route unauthenticated for exactly that reason.

It covers the admin surface only. Member (`/api/v1/cli/*`) and runtime types
stay hand-maintained in `mobile/src/lib/types.ts` until the gateway annotates
those routers — that follow-up belongs to the gateway repo.

### What is generated from it

`mobile/src/generated/gatewayAdmin.ts`, via `openapi-typescript` (pinned exactly
in `mobile/package.json`). Both the snapshot and the generated file are
committed, on the same pattern as `mobile/src/generated/wire.ts`.

### Refreshing it

The snapshot is a **pinned contract**. It moves when the gateway's admin API
changes and we choose to adopt the change — not on every gateway deploy — so CI
checks the generated types against the committed snapshot and deliberately does
not compare the snapshot against a live gateway. A stale snapshot is a
reviewable diff, not a build-time surprise.

Point the refresh at any gateway serving the document. The documented path is a
local dev rig, in a model-gateway checkout:

```sh
make dev-seed && make dev     # Postgres on 55433, gateway on 127.0.0.1:28081
```

then, from this repository:

```sh
pnpm --dir mobile refresh-gateway-openapi                      # default 127.0.0.1:28081
```

To point at a different gateway, run the script directly:

```sh
node mobile/scripts/sync-gateway-openapi.mjs --fetch=https://gateway.example.com
```

Either form rewrites the snapshot (keys sorted, two-space indent, so the diff
tracks the contract rather than the serializer) and regenerates the types.

To regenerate the types alone from the committed snapshot:

```sh
pnpm --dir mobile sync-gateway-openapi
```

CI runs `pnpm --dir mobile check-gateway-openapi`, which rebuilds both artifacts
in memory and fails if either differs from what is committed.
