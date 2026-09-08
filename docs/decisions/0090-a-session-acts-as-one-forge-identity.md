# 90. A session acts as one forge identity, fixed at start

- Status: Accepted
- Date: 2026-09-08
- Owners: thet
- Related: [0063](0063-hosted-machines-borrow-forge-credentials.md); [0065](0065-hosted-git-acts-as-the-person.md); [0088](0088-a-slack-session-runs-where-the-deployment-can-run-it.md); [0089](0089-service-principals.md); `docs/gateway-boundary.md`; `docs/slack-sessions.md`
- Supersedes: none

## Context

Hosted machines borrow forge credentials per call. The gateway now accepts
`attribution` = absent | `person` | `installation` and answers the App's bot
for `installation`. Until this record, Tidebreak always asked for the person.
A service principal owns sessions without signing in (decision 89), and those
sessions should act as the deployment's bot, not as a person who is not
there. Execution location is already chosen once at session start (decision
88). The forge identity belongs in the same place.

## Decision

**A session acts as one forge identity, fixed at start.** Store it on the
session as `acts_as` (`person` or `bot`). Null on an existing row means the
owner-kind default: a person's session acts as themselves, a service
principal's as the bot. The choice never changes for the session's life.

**The borrow names the request.** `GitCredentialLender` takes
`GitForgeAttributionRequest` (`person` or `installation`) on the identity
probe, the credential mint, and the repository list. The gateway answers
that identity or refuses it by name (`person_not_offered` when the session
asked to act as you and the forge cannot).

This slice does not read the channel's request. External get-or-create keeps
today's behavior; a following slice can take the adapter's choice.

## Amendment 2026-09-08: get-or-create names who the session acts as

External get-or-create now takes optional `acts_as` and decides once, before
the workspace is cloned. A service-owned session always acts as the bot. A
request for `bot` acts as the bot. `person` or an absent value probes the
delegated lender as the person: a person identity is kept; `not_connected`,
`person_not_offered`, or `no_git_forge` falls back to the bot and, when the
person could connect, returns `connect_url`; `unavailable` is `502
forge_unavailable` and starts nothing. A machine with no lender keeps today's
person semantics and does not probe.

## Alternatives considered

**Decide from the channel at each borrow.** Rejected: a session that starts
as you and later pushes as the bot would surprise the person who connected
it, and the transcript would not say which identity ran.

**Always ask for the person.** Rejected: a service-owned session has no
person to act as, and the gateway already serves the bot on request.

## Consequences

Hosted git and `gh` on a machine session borrow as the session's identity.
A bot session does not write a personal `user.name` on the checkout; the
App's bot login is the identity the gateway's probe answers. The snapshot
reports `acts_as` so the desktop can say "as you" or "as the bot". Local
machines with no lender stay unchanged.

## Validation

- A person session's loopback borrow records a `person` request; a bot
  session's records `installation`.
- The gateway client sends `person` or `installation` on the probe, mint,
  and list.
- A null `acts_as` column reads as person for a person owner and bot for a
  service owner.
- Delivery under a bot session names the App's login.
