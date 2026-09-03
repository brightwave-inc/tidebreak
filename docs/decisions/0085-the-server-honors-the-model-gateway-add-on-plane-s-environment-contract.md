# 85. The server honors the Model Gateway add-on plane's environment contract

- Status: Accepted
- Date: 2026-09-03
- Owners: thet
- Related: [0047](0047-gateway-linked-hosting.md); Model Gateway ADR 0095 (managed add-on hosting) and ADR 0108 (release tracking)
- Supersedes: none

## Context

A gateway-hosted Tidebreak machine is a managed add-on of the Model Gateway
add-on plane. The plane hands every managed workload one environment
contract: `GATEWAY_BASE_URL`, `GATEWAY_CLIENT_ID`, `GATEWAY_CLIENT_SECRET`,
`ADD_ON_PUBLIC_URL`, and, when the row declares a database, `DATABASE_URL`.
The server reads only its own names: `TIDEBREAK_AUTH_GATEWAY_URL`,
`TIDEBREAK_PUBLIC_URL`, `TIDEBREAK_DATABASE_URL`.

The devops machine has run since 2026-08-27 only because those three
`TIDEBREAK_*` variables were patched onto its Deployment by hand with
kubectl. Server-side apply preserves another manager's fields, so the patch
survived every rollout, but a remove and reinstall of the row would drop it
and the machine would boot without a database URL or a gateway to
authenticate against. The plane now tracks Tidebreak's own releases and
adopts each one on its own (Model Gateway ADR 0108), so the machine is
rolled far more often than a person looks at it.

## Decision

In `Config::from_env`, and in `database_url` for the self-host profile, the
plane's name stands in when the server's own name is unset or blank:

| Server variable | Plane variable |
| --- | --- |
| `TIDEBREAK_AUTH_GATEWAY_URL` | `GATEWAY_BASE_URL` |
| `TIDEBREAK_PUBLIC_URL` | `ADD_ON_PUBLIC_URL` |
| `TIDEBREAK_DATABASE_URL` | `DATABASE_URL` |

The `TIDEBREAK_*` name always wins when both are set. Nothing else from the
contract is read: `GATEWAY_CLIENT_ID` and `GATEWAY_CLIENT_SECRET` are the
plane's credential for the add-on registration, which the server does not
use, and the blob store, listen address, and profile keep their own names
and their documented defaults (the image sets `TIDEBREAK_PROFILE`,
`TIDEBREAK_LISTEN_ADDR`, and `TIDEBREAK_UI_DIST`).

`ADD_ON_PUBLIC_URL` carries a trailing slash, byte-identical to the plane's
machine-binding canonical; `canonical_public_url` already trims it, so the
OAuth resource derived from it is the one the patched value produced.

## Alternatives Considered

- **Keep the kubectl patch.** It is invisible to the plane, to the console,
  and to anyone reading the row, and it dies with the first reinstall.
- **Have the plane emit `TIDEBREAK_*` names.** The plane is add-on-agnostic
  by design (Model Gateway ADR 0095 decision 5); a per-add-on rename in the
  plane would be the first, and every add-on after Tidebreak would want its
  own.
- **Write the three names through the row's environment write.** Works for
  the URLs, but `DATABASE_URL` is a plane-minted Secret reference the
  environment write cannot express, and it repeats what the plane already
  states.

## Consequences

A machine the plane installs from its preset boots with no environment
write at all. The hand patch on devops can be removed once a server image
carrying this decision runs there. A self-host operator who runs Tidebreak
outside the plane sees no change: none of the plane names are set in their
environment. The decision is revisited if the plane's contract renames a
variable, or if the server ever needs the plane's client credential.

## Validation

- `plane_fallback` is a pure function with a unit test covering unset,
  blank, and both-set inputs; a wrong implementation that let the plane's
  value override a set `TIDEBREAK_*` fails the both-set case.
- On devops, after the image carrying this ships: strip the kubectl patch
  from the Deployment and confirm the pod still authenticates against the
  gateway and opens its store.
