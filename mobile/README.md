# Tidebreak mobile

Supervision-first Expo client for hosted Tidebreak. This slice pairs the phone
with a Model Gateway deployment, attaches to the advertised Tidebreak machine,
and supervises existing code sessions: live timelines, pending approvals,
approve or deny-with-feedback decisions, steering, interrupts, and follow-up
turns. You can also open existing chats and attach a Tidebreak machine
directly with a URL and token. This is a preview for Model Gateway and
self-hosted machines, not a client for a local-only desktop install.

The app lives here, outside the Cargo workspace. It does not share the desktop
UI package.

## Run

Requires Node 20+ and pnpm.

```sh
cd mobile
pnpm install
pnpm start
```

`pnpm start` is `expo start`. Press `i` / `a` for the iOS or Android simulator,
or scan the QR code with Expo Go.

Checks used in CI:

```sh
pnpm typecheck
pnpm lint
pnpm test
```

Shipping to TestFlight: see [`DEPLOYING.md`](DEPLOYING.md).

## Generated types

`src/generated/` is committed output. Do not hand-edit it; CI regenerates and
fails on any difference.

| File | Source | Regenerate |
| --- | --- | --- |
| `wire.ts` | the desktop UI's generated wire types, byte-identical | `pnpm sync-wire` |
| `gatewayAdmin.ts` | `schemas/gateway-admin-openapi.json`, via `openapi-typescript` | `pnpm sync-gateway-openapi` |

The gateway admin snapshot is a pinned contract refreshed on demand, not on
every gateway deploy — see [`schemas/README.md`](schemas/README.md) for where it
comes from and how to refresh it. `src/lib/gatewayAdmin.ts` holds the named
aliases screens should import instead of indexing the generated file directly.

## Pairing

1. Enter the gateway public base URL.
2. The app calls unauthenticated `GET /api/v1/meta` and stores
   `tidebreak_machine_url` as the machine prefill when present. The same
   response decides the OAuth scope to request — see Connections below.
3. The system browser opens `{gateway}/oauth/authorize` as public client
   `tidebreak-mobile` with PKCE S256. The redirect is the app scheme plus
   `://callback` (`tidebreak://callback` in production).
4. The authorization code is exchanged at `{gateway}/oauth/token`. Refresh
   tokens rotate; the resources this client will mint are `control`,
   `control_plane`, `tidebreak:<hex>`, and `runtime:<slug>`.
5. When the gateway advertised a machine URL, the app attaches to it
   automatically and lands on the hub. There is no confirm step: attach
   validation refuses any machine but the paired deployment's own, so
   confirming a prefilled field decides nothing.

### QR / console pairing

An alternative to steps 1–4 for a gateway whose console shows a pairing code
(mg ADR 0086). Scan it, or open a `tidebreak://provision?gateway=…&session=…`
link:

1. The payload carries a gateway URL and a pairing-session handle. Neither is
   a credential, so an intercepted code yields only a claim the console user
   can see and refuse. `src/lib/provision.ts` is the allowlist: this app's own
   three schemes, host `provision`, an `mg_ps_` handle, and a gateway URL that
   passes the same validation a typed one does.
2. The phone claims the session with a fresh PKCE challenge, declaring
   `client_id=tidebreak-mobile` — omitting it would mean `tidewatch`, whose
   registered redirects are not this app's.
3. Both screens show the same four-character match code, derived from the
   challenge exactly as the gateway derives it. If they differ, deny at the
   console.
4. On approval a single poll receives an authorization code, redeemed through
   the same grant as the browser path — so both paths produce one identical
   connection.

## Notifications

Push rides the gateway (mg ADR 0093) and is offered only where
`GET /api/v1/meta` advertises `surfaces.push`.

- Registration is per connection: each paired gateway holds its own push
  address for this phone, minted with that connection's own `control` bearer
  and reconciled on every return to the foreground.
- Per-kind toggles live in Settings. They are per *account*, not per device —
  suppression happens at the gateway's enqueue site.
- Signing out deregisters the device **before** clearing the credential; the
  DELETE needs a bearer the connection is about to stop being able to mint.
- Decision kinds carry action buttons (Nudge / Cancel / Accept & stop). They
  use the owner-scoped runtime verbs, so a session whose grant lacks
  `runtime:execute` degrades to a tray message pointing back at the app rather
  than firing a request the gateway would refuse. The `runtime:<slug>` audience
  is resolved through `runtimeSlug.ts`, so a member on an installation that
  serves no MCP endpoint can still cancel and steer their own runs from the
  tray. A run that ended between the push and the press reads as "Already
  finished" rather than as a failure.
- **Android is display-form only today.** This repository ships no
  `google-services.json`, so an Android build has no FCM registration, never
  claims `renders_data_messages`, and therefore receives ordinary tap-only
  notifications. The background renderer that attaches the buttons is present
  and arms itself; it stays silent until the Firebase config lands. iOS is
  unaffected — its buttons come from the OS-native category match.

## Connections

The app holds several connections at once and one is active
(decision [97](../docs/decisions/0097-connections-are-plural-and-typed.md)).
A connection has a kind: `gateway` is an OAuth pairing, and `machine` is a
direct URL with a static token from that machine's roster. The welcome
screen and Settings → Connections both offer that path.

- Each connection keeps its own credential under its own secure-store key, so
  signing out of one leaves the others signed in. Access tokens stay in memory.
- Settings → Gateway opens the connection list: switch, sign out, or pair
  another gateway.
- Upgrading from an older build migrates its single stored session into a
  connection, keeping the pairing and its rotating refresh token.
- A reinstall wipes stored credentials: iOS Keychain entries survive an
  uninstall, and the recorded install time is how the app notices.
- Scope escalation is gated on advertisement. The baseline stays
  `openid profile offline_access`; `control_plane:read` and `runtime:execute`
  are requested only from a gateway advertising
  `surfaces.tidebreak_mobile_console`, and `control_plane:write` also needs
  `surfaces.control_plane_write`. No gateway advertises the first flag yet
  (brightwave-inc/model-gateway#2044), so today every pairing is machine-only.
  A gateway that refuses a console resource answers `invalid_resource`, which
  is a degraded surface rather than a sign-out.

Attach validates the machine URL the same way desktop does, reads
`/auth/discovery`, derives `tidebreak:<sha256(canonical_url)>` locally, and
refuses a mismatched echo or a gateway URL that is not the paired deployment.
Discovery also carries the machine's `version` and `api_level`. The app
compares the level with the range in its copy of `wire.ts` (`MIN_API_LEVEL`
through `API_LEVEL`) and refuses a machine outside it with a message that says
whether to update the app or the machine. A machine that predates the
handshake leaves both keys out and attaches as before. `GET /policy` is the
authenticated probe. Auto-attach runs exactly this sequence — nothing is
skipped but the tap.

The Attach screen is the fallback, and it is where the app lands whenever
auto-attach cannot finish:

- the gateway advertised no `tidebreak_machine_url` (enter one),
- discovery timed out after 10s — usually a hosted machine on a VPN this phone
  isn't on, shown with that hint and a Retry,
- the echo or gateway URL failed validation,
- the machine runs an API level this app does not read,
- or the machine URL needs correcting by hand.

## Environment variants

`APP_VARIANT` selects the native scheme and bundle id:

| `APP_VARIANT` | Scheme | Redirect |
| --- | --- | --- |
| unset / `production` | `tidebreak` | `tidebreak://callback` |
| `staging` | `tidebreak-staging` | `tidebreak-staging://callback` |
| `development` | `tidebreak-dev` | `tidebreak-dev://callback` |

Example:

```sh
APP_VARIANT=development pnpm start
```

Do not put tokens, secrets, or internal hostnames in this tree.
