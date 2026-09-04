# 87. Standalone browser sign-in

- Status: Accepted
- Date: 2026-09-04
- Owners: thet
- Related: [0006](0006-self-host-deployment-plane-authorization.md) (sequences roster-provisioned tokens before an authenticator); [0049](0049-gateway-authenticated-hosted-machines.md) (fail-closed rules); [0082](0082-the-hosted-machine-serves-the-renderer.md) (the machine serves the page; a bearer arrives in the fragment); `docs/self-hosting.md`; tidebreak #3178 (track B), #3182
- Supersedes: none

## Context

A gateway-authenticated machine signs a browser in through the console's
hand-off: the console mints a one-time code, the machine redeems it, and
the page receives a bearer in the URL fragment. A standalone machine, one
on `TIDEBREAK_AUTH_TOKENS_FILE`, has no console and therefore no browser
sign-in at all. The page says so and the desktop app stays its only client.
A Slack link to a session on such a machine lands on a screen that cannot
be passed, which makes every Slack feature this epic builds a
gateway-only feature by accident.

Decision 6 already sequences the end state: tokens provisioned from a
roster first, then an authenticator behind the seam that turns a credential
into a principal. What is missing is the browser half of both.

## Decision

A standalone machine signs a browser in by one of two paths, chosen by the
machine's boot mode and reported in its discovery document.

- **Token paste.** In `static_token` mode the sign-in page takes a token,
  probes the principal read with it, and on success holds it in memory for
  the tab's life exactly as the hand-off bearer is held: no cookie, no
  storage, gone on reload. The page honors `return_to`, so a session link
  lands on its route after sign-in. This is the bootstrap path and the CLI
  path; it is not retired by the second.
- **OIDC.** A third exclusive boot mode, selected by
  `TIDEBREAK_AUTH_OIDC_ISSUER`, client id, and client secret, with an
  optional login claim override. The machine starts an authorization-code
  flow with PKCE, verifies the callback's state and nonce, validates the ID
  token against the issuer's discovery document and keys, maps the login
  claim to a principal, mints a machine-issued bearer bound to that
  principal for one hour, and lands the page through the same fragment
  envelope the hand-off uses. Discovery reports `mode: oidc` and the start
  URL; the page shows one button.
- **Exclusivity and refusals.** A machine is gateway, static-token, or
  OIDC; combining the gateway URL with OIDC is a boot error. The token file
  may sit beside OIDC as the bootstrap for the first admin and for CLI
  access. An OIDC machine refuses a static token; a token-file machine
  refuses an OIDC bearer; a provider refusal admits nobody.

Everything above the credential-to-principal seam stays unaware of which
mode is in use, which is decision 6's requirement.

## Alternatives Considered

**Require a gateway.** Every Slack feature would need a gateway
installation. Rejected: the plan's premise is that Tidebreak works without
one, and the machine engine is the floor.

**A machine-local password login.** Rejected: it creates a second identity
store to provision, rotate, and audit. The token file already is the local
roster; OIDC is where real identity lives.

**A bearer in a cookie.** Rejected for the same reasons decision 82 gave:
a fragment never reaches a server or its logs, and the page clears it before
the router runs.

**Only OIDC, no token paste.** Rejected: the first admin of a standalone
machine has no OIDC principal yet, and a CLI session needs a token either
way.

## Consequences

`auth.rs` gains a third authenticator and two routes; the discovery
document gains a mode and a start URL; `docs/self-hosting.md` loses its
"no browser sign-in" paragraph. The OIDC path adds a dependency on the
issuer's availability at sign-in time and nothing else, because the bearer
is machine-issued once the ID token is verified.

A pasted token is as strong as the file it came from; the roster owner
decides who holds one. A stale OIDC mapping (a login claim that changes)
signs a person in as nobody, which is the fail-closed outcome.

Revisit when the gateway's principal read grows team ids and the standalone
roster wants the same shape, or when a refresh flow is wanted for browsers
kept open past an hour (tracked separately as hosted tab re-entry).

## Validation

- A fresh browser opening a session link on a static-token machine is
  asked for a token and lands on the session; a wrong token stays on the
  screen with the refusal.
- On an OIDC machine the same link goes through the issuer and lands on
  the session; a callback with a mismatched state, nonce, or audience is
  refused and nothing is minted.
- An OIDC machine refuses a static token; a token-file machine refuses an
  OIDC bearer.
- Nothing about a bearer or a token is written to storage, a cookie, or a
  log line; the discovery document is readable without a bearer.
