# 14. Shared-App Publishing Is a Gateway Governance Action

- Status: Accepted
- Date: 2026-08-12
- Owners: desktop, server
- Related: record 10 (gateway connected-app bindings for local apps),
  [`docs/connected-apps.md`](../connected-apps.md),
  [`docs/local-apps.md`](../local-apps.md), the model gateway's shared-apps
  design (its ADR 0036)
- Supersedes: none

## Context

A local app authored on a gateway-managed profile is auto-registered as a
draft at the gateway, and the gateway stores everything sharing-related:
the draft and its revisions, the published status, and the per-team grants
that make it openable (`shared_app_team_grants`, written idempotently — a
repeat publish to the same team changes nothing). Revoking a grant,
disabling an app, and auditing all of it already live on the gateway's own
surfaces.

Publishing alone lives in the harness: OpenWave ships a Publish dialog — a
team picker plus a preflight confirmation — that calls the gateway's
CLI-tier publish route. The first real end-to-end use showed why that
placement misleads. The picker is stateless: the gateway's CLI tier exposes
no read of an app's existing team grants, so after publishing, the same
dialog re-offers the same team as if nothing had happened, and nothing in
the harness can say "already published to Example Engineering — publishing
again sends the current revision". Fixing that in place means a new
CLI-tier read plus disclosure UI — rebuilt in OpenWave today and in every
future harness that authors apps, each copy a second interface of record
that can drift from the gateway's own pages.

The underlying misfit: publishing mutates gateway-owned entitlement state.
Every other mutation of that state is done on the gateway; the harness
holds the one exception.

## Decision

Publishing — and re-publishing a later revision — is a governance action
done on the gateway's own web surface, alongside the publish state,
grants, and revocation it belongs with. Harnesses author, revise, and
auto-register drafts; no harness product surface mutates publish state.

- OpenWave's Publish affordance becomes a link that opens the app's page
  on the gateway (built from the managed policy's gateway URL), where
  status and grants are native. The team picker and preflight dialog are
  removed.
- The gateway grows the author-facing publish affordance on that page; it
  is viewer-only today.
- The CLI-tier publish route remains as API — the containment is of
  product surface, not capability. Scripted and test use continues; no
  harness UI calls it.
- Deliberately excluded: syncing publish state into harnesses, and a
  CLI-tier team-grants read for picker disclosure. The problem those would
  solve no longer exists.

## Alternatives Considered

- **Do nothing.** Mechanically safe — the grant write is idempotent — but
  the dialog actively misleads authors about what publishing again does,
  and the confusion was hit on first real use.
- **Disclose publish state in the harness picker.** A new CLI-tier grants
  read plus picker UI ("published to X · r1"). Fixes the confusion but
  entrenches the misfit: every harness re-implements a governance surface,
  each a cache of gateway state that can go stale, and revocation still
  lives on the gateway, so authors manage sharing in two places forever.
- **Remove the CLI-tier publish route as well.** The cleanest reading of
  "publishing is governance", but it breaks scripted publishing (test
  fixtures and automation drive it today) for no product gain, since the
  route already requires the caller's own gateway authority.

## Consequences

Two implementation moves, gateway UI first: the gateway app page gains
publish/re-publish for the author, then OpenWave swaps its dialog for the
link. Until the swap lands, the existing dialog stays — a link pointing at
a page with no publish control would strand authors. The harness loses a
same-window flow: publishing now means a browser hop, which is judged
acceptable because sharing is infrequent and the destination shows the
state the dialog could not. Harness code no longer needs the CLI-tier
teams read for the picker.

Revisit if a harness needs publishing where the gateway web UI is
unreachable (batch or offline flows), or if automation demand shows the
UI-only containment rule is drawing the line in the wrong place.

## Validation

- The gateway page shows current publish state next to the action; a
  repeat publish is presented as re-sending the current revision, not as a
  first publish — the exact confusion that motivated this record.
- OpenWave contains no code path that calls the CLI-tier publish route;
  the affordance opens the gateway page and nothing else. A plausible
  wrong implementation keeps the dialog behind a feature flag — the flag's
  removal is part of the swap, not a follow-up.
