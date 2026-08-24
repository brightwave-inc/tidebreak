# 65. Hosted git and pull requests act as the person

- Status: Proposed
- Date: 2026-08-24
- Owners: server, desktop
- Related: [`0034-harness-discovery-credentials.md`](0034-harness-discovery-credentials.md),
  [`0049-gateway-authenticated-hosted-machines.md`](0049-gateway-authenticated-hosted-machines.md),
  [`0063-hosted-machines-borrow-forge-credentials.md`](0063-hosted-machines-borrow-forge-credentials.md),
  [`../gateway-boundary.md`](../gateway-boundary.md)

## Context

On a local machine, work lands as the person. Decision 34 observes the
caller's own `gh` login and git configuration, so a commit, a push, and a
pull request all carry the identity of whoever drove them. Decision 63 gave
hosted machines their first GitHub identity — per-operation installation
tokens from the deployment's GitHub App — and work landed as the App's bot
account, because installation tokens were the one credential the gateway
could mint without asking any person to authorize anything.

Bot attribution is a stopgap, not the product. The person who drives a
hosted workspace is accountable for the work: they steer the agent, review
the diff, and see the pull request through, even when the AI writes and
pushes every line. Credit and review conventions key on the author —
CODEOWNERS, approve-your-own-PR rules, contribution history — and a hosted
machine should read exactly like the same person working locally.

The mechanism exists. The deployment's forge app is a GitHub App, and a
GitHub App mints two credential kinds: installation tokens, which act as
the App, and user access tokens, which act as a person who authorized the
App once. The forge app's OAuth client is already registered with the
gateway, people already authorize this same forge and drive gateway
sandboxes as themselves with it, and the gateway already distinguishes
personal from installation credentials per app. The gateway's git
surfaces grow a personal mode (successor to gateway ADR 0083): the
git-credential mint returns the caller's user token, and the git-forge
probe answers the caller's own connection state.

Decision 63 rejected lending a person's identity to a shared machine
because work would land as a person who never acted from their own device.
That objection is about unattended and cross-user lending, and it stands.
It does not describe this record: every borrow is driven by the caller's
own live, machine-bound session — the person is acting from their own
device, through the machine that hosts their workspace — and the gateway
audits every mint to them.

## Decision

1. **The borrowed credential is the caller's own user token.** On a
   gateway-authenticated hosted machine, a clone, a push, and a pull-request
   operation borrow a short-lived GitHub user access token minted for the
   requesting caller. Decision 63's handling discipline is unchanged: held
   in memory for the one operation, injected through a one-shot credential
   helper over an emptied helper list, answered only for `https` on the
   forge's exact host, never stored. A user token cannot be scoped to one
   repository the way an installation token can — its ceiling is the App's
   permission set intersected with the person's own repository access — so
   the App's permissions stay minimal and the host gates carry the
   confinement.

2. **Authorization is explicit, once per person, and revocable.** A person
   authorizes the deployment's forge App through the gateway's existing
   delegated-OAuth flow. The per-caller probe then answers connected,
   naming the login work will land as. A caller who has not authorized
   reads "not offered", with connecting as the remediation — never a silent
   fall back to another identity. Disconnecting at the gateway ends the
   machine's ability to act as them at the next borrow.

3. **Commits carry the person's identity.** A hosted workspace configures
   its git author and committer from the caller's GitHub account when the
   workspace is created, so history names the person rather than an
   image-wide environment identity.

4. **Pull requests ride REST on the same borrowed token.** Pull-request
   creation and the delivery reads stop requiring `gh` and drive the
   forge's REST API with the same per-operation credential. A pull request
   opened from a hosted machine is authored by the person.

5. **The UI still names the acting identity.** The attribution seam from
   decision 63 stays: the add-repository source and the workspace git card
   say whose identity acts — now the caller's own login. Legibility was the
   bargain for bot attribution, and it holds for person attribution too.

## Rejected

- **Keeping the App's bot identity for user-driven work.** Misattributes
  credit and accountability, and breaks every author-keyed convention.
  Decision 63's bot attribution stands only until this ships, and remains
  the honest shape for work no person drove.
- **Falling back to the bot when a person has not connected.** Two
  attribution states behind one push button; a reader could not know which
  identity acted without checking. Not-offered-with-remediation is legible.
- **Person-authored commits pushed by the bot.** Setting the person as
  commit author while the bot pushes and opens the pull request splits
  attribution across two identities and forges authorship without the
  person's consent.
- **Registering a second, installation-mode forge app.** The previous
  slice's remaining checklist step. Unnecessary now: the existing forge App
  already carries the OAuth client this record needs, and the machine
  deliberately serves exactly one forge.

## Revisit when

- Unattended hosted work — triggers, scheduled jobs, anything with no live
  caller session — needs a git identity. Installation tokens are the right
  shape for it, and decision 63's machinery is already built.
- A deployment needs a policy forcing bot attribution (environments where
  individual identity must not appear on the forge); that is a gateway
  policy switch over the same surfaces.
- The forge is not GitHub. User-token mechanics are App-specific, and
  another forge kind needs its own consent story.
- The harness agent's own `git push` inside a hosted workspace should act
  as the person too. That needs a credential seam through shell policy,
  which denies helper configuration by design today; open it deliberately,
  not as a side effect of this record.
