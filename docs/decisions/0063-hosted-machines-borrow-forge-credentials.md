# 63. Hosted machines borrow repository-scoped forge credentials

- Status: Proposed
- Date: 2026-08-23
- Owners: server, desktop
- Related: [`0034-harness-discovery-credentials.md`](0034-harness-discovery-credentials.md),
  [`0049-gateway-authenticated-hosted-machines.md`](0049-gateway-authenticated-hosted-machines.md),
  [`0062-per-caller-gateway-entitlements.md`](0062-per-caller-gateway-entitlements.md),
  [`../gateway-boundary.md`](../gateway-boundary.md)

## Context

A hosted machine has no GitHub identity. Decision 34's posture — credentials
are observed, never brokered — is right for a machine on someone's desk,
where the person's own `gh` and git configuration answer every question. On a
shared hosted machine there is nothing to observe: no `gh`, no credential
helper, no person at the keyboard. A team cannot clone its private
repositories, and the repo-source probe honestly reports a GitHub source that
only reaches public repositories.

The deployment's gateway can already hold the right identity. A git-forge
connected app in `github_app_installation` mode stores a GitHub App and mints
hour-long installation tokens with no standing secret, and gateway ADR 0083
gives hosted machines two surfaces over it, both authenticated with the
caller's live machine-bound token: `POST /api/v1/tidebreak/git-credential`
mints a token scoped to one named repository, and
`GET /api/v1/tidebreak/git-forge` answers availability and the App's identity
without minting. Work done this way lands as the App's bot account — that is
installation mode's documented posture, and a deployment whose forge policy
is `installation_only` cannot attribute it to the person.

## Decision

1. **Each git operation borrows a dying credential.** On a
   gateway-authenticated hosted machine, a GitHub clone and a workspace push
   each present the caller's machine-bound token to the gateway and receive
   an installation token scoped to the one repository the operation touches.
   The credential lives in process memory for the length of that operation,
   travels to git through the environment into a one-shot credential helper,
   and is never written to the store, the checkout's configuration, or the
   persisted clone URL. The helper configuration first empties git's helper
   list, so no configured helper can answer ahead of the borrowed credential
   or store it when git offers it back.

   The credential is confined to the forge's own host, twice over. The
   machine lends only for an origin whose host is the forge's — the origin
   URL is workspace state an agent can rewrite, and without the gate the
   next push would offer a live token to whatever host `origin` names. And
   the helper itself answers `get` only for `https` and that exact host, so
   a rewrite or a redirect that slips past the first check still collects
   nothing.

2. **The repo-source probe answers per caller, from the gateway.** On a
   hosted machine the `github` source stops consulting `gh` — there is none —
   and reflects the gateway's probe for the requesting caller:
   offered when an entitled installation-mode forge would serve them, and
   otherwise hidden with the gateway's own reason. A deployment with no
   forge, a person-bound (Connect-mode) forge, an uninstalled forge app, and
   an ambiguous pair each read as "not offered, because…", never as an error.
   Local machines keep decision 34's observation exactly as it was.

3. **The UI says whose identity acts.** The add-repository dialog's GitHub
   source carries the attribution sentence — work lands as the App's bot
   account, not the person — and the workspace git card names the acting
   identity (`pushes_as`) beside the push control. Legibility is the
   product's half of installation mode's bargain: the gateway audits every
   mint to the caller, and the machine states on screen that authorship
   belongs to the App.

4. **Refusals fail the operation, with the gateway's reason.** A mint
   refusal — no forge, a repository outside the installation, a dead session
   — fails the clone or push with that reason rather than retrying without a
   credential. An uncredentialed retry would fail with a worse message and
   blur which identity acted. A checkout whose origin is not a lendable
   forge repository — a local path, a bare test origin, any host but the
   forge's — borrows nothing and pushes exactly as it does today.

5. **Nothing else changes.** Static-token self-host machines and desktops
   never build the lender, so every existing git and `gh` path is untouched.
   Pull-request creation and the delivery reads still ride `gh` and remain
   unavailable on hosted machines; they are the next slice, not this one.

## Rejected

- **Storing a GitHub credential on the machine.** A standing secret the
  gateway cannot revoke or audit, acting as one identity for every member.
  Rejected by the gateway's ADR 0083 and by decision 34's posture here.
- **Lending a person's Connect-mode identity to the machine.** Work would
  land as a person who never acted from their own device. The gateway
  refuses this mode by name; the deployment registers a second,
  installation-mode forge app instead.
- **Probing availability by minting against a sentinel repository.** Costs a
  forge round-trip per probe and can mint a real credential when the
  sentinel exists. The gateway answers availability from storage; the
  machine asks that surface.
- **Caching borrowed credentials to expiry.** One fewer mint per hour per
  repository, but an hour-long secret held in memory beside every workspace,
  for operations that happen seconds apart at most. Minting per operation
  keeps the holding window as short as the operation itself.

## Revisit when

- Hosted machines need pull-request creation and delivery reads; those need
  a REST path driven by the same borrowed credential, not `gh`.
- The harness agent's own `git push` inside a workspace should work on
  hosted machines; that needs a credential-helper seam into agent-run git,
  which shell policy currently denies by design.
- A deployment registers two forges and the gateway grows its explicit
  selector; the machine's requests would then need to name one.
