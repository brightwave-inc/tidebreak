# 98. Standalone machine attach on mobile

- Status: Proposed
- Date: 2026-09-15
- Owners: mobile
- Related: [`0097-connections-are-plural-and-typed.md`](0097-connections-are-plural-and-typed.md)
  (the `machine` kind this record builds),
  [`0087-standalone-browser-sign-in.md`](0087-standalone-browser-sign-in.md)
  (the token-paste path and the `/auth/token-sign-in` probe),
  [`0072-mobile-client.md`](0072-mobile-client.md),
  [`0006-self-host-deployment-plane-authorization.md`](0006-self-host-deployment-plane-authorization.md)
  (the roster the token comes from), GitHub #3404, epic #3398
- Supersedes: none

## Context

A Tidebreak machine can run with no Model Gateway in front of it. Booted with
`TIDEBREAK_AUTH_TOKENS_FILE`, it authenticates with long-lived named bearer
tokens from an operator-maintained roster and advertises
`{"mode":"static_token"}` on `GET /auth/discovery`
(`crates/tidebreak-server/src/auth.rs`, `AuthDiscovery` and `TokenMap`). The
desktop app and the browser sign-in page both speak to such a machine already;
the phone refuses every discovery mode except `gateway`.

Decision 97 left the `machine` connection kind declared and unbuilt precisely
so this slice could add a member rather than re-cut the store. What it did not
settle is the part that is genuinely open, and that the epic named as the open
question: **transport**. `validatedBaseUrl` — the rule this app shares with the
desktop — refuses `http://` to anything but a loopback host. A machine on a
home or office LAN typically has no certificate a phone already trusts, so the
common standalone deployment is exactly the one the rule refuses.

Three facts bound what can be done about that.

**Expo exposes no TLS hooks.** The app's transport is `expo/fetch`
(`src/lib/http.ts`), chosen because it is the only one that can refuse a
redirect. Neither it nor React Native's `WebSocket` offers a certificate
callback, a trust-evaluation delegate, or a pinning API from JavaScript.
Accepting a self-signed certificate on iOS means a native
`URLSessionDelegate` implementing `urlSession(_:didReceive:completionHandler:)`;
on Android it means a custom `X509TrustManager` in the OkHttp client React
Native builds. Both are native modules, both apply to the whole app rather than
to one attach, and both must be kept in step with two transports (`expo/fetch`
and the WebSocket) that do not share a client.

**Any user-accepted-fingerprint design is a new trust decision, made by a
person who has no way to verify it.** The person attaching is standing in front
of a phone, not the machine. A fingerprint they are asked to approve is one
they read off the same network they would be trusting.

**Some standalone identity information simply is not published.** A
`static_token` discovery document carries no installation id, no name, no
version, and no resource. There is nothing to bind a certificate to, and
nothing to recognize a machine by other than the origin the phone dialed.

Two smaller questions ride along, because both are wire-shaped and both would
otherwise be settled twice: what a scannable attach code contains, and where
this path appears in onboarding.

## Decision

### 1. Transport: trusted TLS, or loopback. No exceptions in this slice.

Standalone attach reuses `validatedBaseUrl` unchanged. `https://` to any host
is accepted; `http://` is accepted only to `localhost`, `127.0.0.1`, or `::1`.
Everything else is refused before a single byte of the token leaves the device,
with the stable reason `requires_tls` and copy that says a LAN machine needs a
certificate the device already trusts.

The app ships **no** certificate pinning, **no** self-signed acceptance, and
**no** user-accepted-fingerprint flow. A deployment that wants to be reachable
from a phone on the LAN gets there the way any other device does: a certificate
from a CA the device trusts, or a trusted internal CA installed as an MDM
profile, or a tunnel that terminates real TLS.

### 2. The QR payload is a credential, and is treated as one.

```text
tidebreak-machine://v1?url=<percent-encoded https base URL>&token=<token>
```

- The scheme `tidebreak-machine:` is **not** registered in `app.json`. The OS
  will never deliver it as a deep link, so no web page, message, or foreign app
  can hand this phone a machine and a token the user did not scan. The gateway
  provision link (`tidebreak://provision?...`) keeps its registered scheme
  because nothing in it is a credential — it carries a claimable handle that a
  person still approves on a console (mg ADR 0086).
- `v1` is the version, carried as the URL host, so a later revision is a
  different string rather than a reinterpreted one.
- The token is validated against the roster's own rules before use — at least
  32 characters from `[A-Za-z0-9._~-]`, the alphabet `auth.rs` requires so one
  string survives both an `Authorization` header and the
  `tidebreak-token.<token>` subprotocol. The URL is validated by rule 1.
- **The payload never becomes a route parameter.** The attach screen hosts its
  own scanner rather than routing through `/scan`, so the token stays in
  component state until it reaches SecureStore. Nothing logs, reflects, or
  echoes it, including error messages.

Pasting a token by hand is equally supported and is the bootstrap path; the QR
is a convenience over a 64-character string, not a second mechanism.

### 3. Attach order, and what each mode means.

Discovery is probed before the credential is sent, and each mode gets its own
refusal because each has a different answer:

| mode | outcome |
| --- | --- |
| `static_token` | attach proceeds |
| `gateway` | refused: pair that gateway instead, which attaches this machine for you |
| `oidc` | refused: the machine resolves only the `tb_oidc_` bearers it mints for a browser, so a roster token names nobody there |
| `local` | refused: a desktop's per-launch token names nobody and is not issuable |

The credential is first sent at `POST /auth/token-sign-in`, the machine's own
public bootstrap probe (decision 87). `204` attaches; `401` is a token the
machine does not know; `403` is a `service` principal, which owns automated
sessions and deliberately does not sign in.

### 4. The connection, its credential, and its revocation.

A `machine` connection's id is derived from its canonical URL (`mc_<digest>`),
because that is the only stable thing a standalone machine offers. Re-attaching
with a rotated token replaces the record rather than stacking a second one.
The token lives under the connection's own SecureStore key, held by
`StaticTokenStore` — the same shape as `TokenStore` with rotation removed.

A static token has no token endpoint at which to learn it was revoked, so
**a `401` from the machine is the signal**. `MachineClient` reports the status
back to the credential that issued it; `StaticTokenStore` treats `401` the way
`TokenStore` treats `invalid_grant` — wipe, signal, and let the registry forget
that one connection. `403` and every other status are left alone: a `403` is an
answer about a principal the machine still recognizes, and signing a member out
for opening an admin-only surface would be wrong.

**A machine client is bound to the connection that built it.** The connection
id is captured at construction and every credential lookup — the mint and the
refusal alike — goes through `tokensFor(id)`, never through an
active-connection lookup. This is not hygiene: every live surface polls, a poll
survives the user switching connections, and resolving a late `401` against
whoever is active at *response* time would sign out a machine whose token was
never on that wire while leaving the refused one signed in and still polling.
It is the shape the push work already uses for the same reason — a tray press
mints against the connection its payload names, so it cannot re-point the app.
A client whose connection has since been signed out mints nothing and says so
rather than borrowing another's credential.

### 5. Onboarding placement.

One line at the foot of the pair screen — "Pairing your own Tidebreak instance?
Tap here" — and one on the connections screen. The gateway path stays visually
and structurally primary. There is no separate brand, no separate entry screen,
and no build variant: the standalone machine is a different way in, not a
different product. #3314's design pass owns where this sits afterwards.

### 6. Gateway-only surfaces are absent by kind, with a stated reason.

`sections.ts` already gates the console on `isGatewayConnection`. This record
adds `sectionUnavailableReason` / `consoleSectionUnavailableReason`, which
distinguish `no_gateway` (this connection has none, and never will) from
`not_granted` (a gateway pairing that lacks the authority). Push is gated by
`supportsPush`: push addresses live at an installation (mg ADR 0093), so a
machine connection registers no device, appears in no reconcile, and is never
what a notification payload names.

## Alternatives Considered

**Ship certificate pinning.** The honest version needs the fingerprint to
arrive out of band — in the QR payload, say — and needs native code on both
platforms feeding two transports that do not share a client. It is buildable,
but it is a native-module slice of its own, and it must not be the thing
standing between this epic and a working standalone attach. Named below as the
revisit trigger.

**A user-accepted fingerprint, trust-on-first-use.** Rejected: the person is at
the phone, not the machine, and would be approving a fingerprint read off the
network they are being asked to trust. It converts a refusal a user can act on
into a prompt they will always accept.

**Allow plain `http://` to private address ranges (RFC 1918, `.local`).**
Rejected: it sends a long-lived bearer in clear text over a network whose other
occupants are exactly the threat, and both platforms would additionally need
ATS and cleartext-traffic exceptions that apply app-wide. A roster token is
valid until an operator edits a file and restarts the machine — far worse to
leak than a ten-minute minted token.

**Do nothing; require a gateway for mobile.** Rejected: it makes the phone a
gateway-only client and contradicts the premise that Tidebreak works without
one (decision 87).

**Reuse `tidebreak://provision` for the attach code.** Rejected: that scheme is
registered, so any page that can open a link could hand this phone a machine
and a token. The two payloads differ in exactly the property that matters —
one is a credential — and giving them one scheme would erase that.

**Route the scanned payload through `/scan` like the gateway flow.** Rejected:
it would put the token in route parameters and therefore in the router's
history. Hosting the scanner on the attach screen costs one small shared
component and keeps the credential in one component's state.

**Probe `/policy` instead of `/auth/token-sign-in`.** Rejected: `/policy` is
an ordinary member route, so it accepts `service` principals and would attach a
phone as an automation. `/auth/token-sign-in` is the purpose-built public probe
and distinguishes "unknown token" from "not a person".

**Ask the machine who the token names and show it.** There is no such route for
a static principal. Rejected as out of scope rather than on merit; see the
gaps below.

## Consequences

- A standalone machine on a LAN is not attachable from a phone unless its
  operator arranges trusted TLS. This is the deliberate cost, and it is the
  most likely reason a user of this path will be stuck.
- The connection list gains a row whose only identity is a hostname. A
  standalone connection shows no display name, no email, and no installation —
  because the machine publishes none.
- Every supervision surface (sessions, approvals, chats, delivery, workspaces)
  works unchanged: a static-token request resolves to the same
  `Principal::User` a gateway request does, and the wire is identical.
- Three server-side facts shape behavior and are **not** changed by this slice:
  1. `static_token` discovery carries no machine identity at all, so the
     connection id is derived from the URL and the UI can only show a host.
  2. `TokenMap` is loaded once at boot and never re-read, so removing a line
     from the token file does not revoke it until the machine restarts. The
     app's revocation handling is correct but only fires once the machine
     actually starts refusing.
  3. A static-token WebSocket is authenticated at upgrade and never
     re-validated (gateway sockets are, every 60s). A socket open at the moment
     of revocation survives until it closes; the HTTP reads running alongside
     every live surface are what detect it.
- Revisit when a native TLS module exists for both platforms and both
  transports — that is what would let a pinned or operator-supplied certificate
  become a supported LAN posture — or when a standalone machine publishes
  enough identity for a certificate to be bound to something, or when the
  server grows roster reload so revocation is immediate.

## Validation

- Discovery answering `static_token` attaches; `gateway`, `oidc`, and `local`
  each refuse with their own reason, and an unknown mode refuses rather than
  falling through to the permissive branch — the case a plausible wrong
  implementation (`mode !== "gateway"` means static) would pass.
- `http://` to a non-loopback host is refused with `requires_tls`, distinct
  from `url_invalid`, **and no request is made** — the token must not reach the
  network before the transport rule is applied.
- `http://localhost` still attaches, so the rule did not silently become
  "HTTPS only" and break development.
- A token failing the roster's shape is refused before any request; a `401` and
  a `403` from the sign-in probe produce different reasons.
- The QR parser accepts only `tidebreak-machine://v1`, and rejects the same
  payload carried on `tidebreak://` — the deep-link scheme an attacker could
  reach. It rejects a short token, a missing token, a plaintext URL, and an
  unknown version.
- Adding a machine connection alongside a gateway leaves both live; signing out
  of either deletes only its own key.
- A `401` through `MachineClient` wipes exactly that machine's token and drops
  exactly that connection, while a `403` and a `500` change nothing.
- Machine A's request is in flight, the active connection is switched to
  machine B, and A's `401` then arrives: **A alone is signed out and B's token
  is untouched and still mints.** The case a plausible wrong implementation —
  one that asks the registry which connection is active when the response lands
  — gets exactly backwards, signing out the connection that was never refused.
- No connection summary the UI mirrors carries `staticToken`, `refreshToken`,
  or a cached access token.
