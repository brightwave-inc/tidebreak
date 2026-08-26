# 72. Supervision-first mobile client

- Status: Proposed
- Date: 2026-08-26
- Owners: mobile
- Related: [`0049-gateway-authenticated-hosted-machines.md`](0049-gateway-authenticated-hosted-machines.md),
  [`0047-gateway-linked-hosting.md`](0047-gateway-linked-hosting.md),
  GitHub epic #2644
- Supersedes: the parked “supervision-first mobile client” entry in
  [`../deferred.md`](../deferred.md)

## Context

Every code-mode capability is already a server route, and the updates channel
is cheap enough for a phone. The parked note treated a supervision-first
mobile client as waiting on a self-host member-client path. Hosted Tidebreak
now authenticates through Model Gateway (decision 49): a public OAuth client
can pair once, mint a machine-bound `tidebreak:<sha256(url)>` access token,
and speak the existing HTTP+WS wire.

The first buildable slice is pairing plus attach, not the full supervision UI.
The client belongs in this repository so the resource derivation, URL
normalization, and token-rotation rules cannot drift from desktop.

## Decision

1. The mobile client lives in-repo at `mobile/`: a self-contained Expo app
   (iOS and Android) with its own lockfile. It is not part of the Cargo
   workspace and does not import the desktop UI package.
2. Auth is gateway OAuth 2.0 authorization-code + PKCE as public client
   `tidebreak-mobile`. The production redirect is `tidebreak://callback`.
   Access tokens are resource-bound; the only resources this client mints are
   `control` (for `/api/v1/cli/me`) and `tidebreak:<identifier>` for a machine
   the user is attaching to.
3. Refresh tokens rotate and are single-use. The client keeps one serialized
   refresh queue per gateway session and single-flights per-resource access
   token mints. HTTP 400/401 on refresh is a clean signed-out state.
4. Machine attach mirrors desktop: normalize the URL, `GET /auth/discovery`,
   independently derive the resource from the canonical URL, require the
   server’s echo to match, require `gateway_url` to match the paired gateway,
   then probe `GET /policy` with the minted bearer. The echoed resource is
   never trusted alone.
5. Cloud-hosted gateway+machine is the first path. A local relay that makes a
   laptop reachable to the same client is a designed-for follow-on, not a
   second auth system.

## Alternatives Considered

- **A separate mobile repository.** Rejected: resource hashing and URL
  normalization must stay identical to `tidebreak_machine_resource` and
  `validated_base_url`. Drift would silently mint the wrong audience.
- **Reuse desktop’s static Tidebreak token file.** Rejected: decision 49
  already replaced standing tokens for hosted machines. A phone should not
  become a second place those files are copied.
- **Trust `/auth/discovery`’s resource field.** Rejected: a malicious machine
  could name another machine’s resource and collect a bearer for it.

## Consequences

- Mobile CI is a separate workflow (`mobile-checks`) so a phone-only change
  does not wait on Rust lanes.
- Operators must register `tidebreak-mobile` and the app-scheme redirect on
  the gateway. Staging and development use distinct schemes.
- Supervision UI (issue #2646) builds on this attach proof, not a new login.
- Revisit if the gateway stops rotating refresh tokens, or if a local-only
  phone client becomes the primary path.

## Validation

- Known vector: `https://tidebreak.example.test` hashes to the same
  `tidebreak:<hex>` as the Rust unit test.
- A discovery echo that does not match the locally derived resource is
  refused.
- Two concurrent access-token callers produce one refresh HTTP request.
- A 401 refresh response leaves no session in secure storage.
