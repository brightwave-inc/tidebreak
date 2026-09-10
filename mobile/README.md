# Tidebreak mobile

Supervision-first Expo client for hosted Tidebreak. This slice pairs the phone
with a Model Gateway deployment, attaches to the advertised Tidebreak machine,
and supervises existing code sessions: live timelines, pending approvals,
approve or deny-with-feedback decisions, steering, interrupts, and follow-up
turns. Workspace/session launch and general chats remain separate later slices.

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

## Pairing

1. Enter the gateway public base URL.
2. The app calls unauthenticated `GET /api/v1/meta` and stores
   `tidebreak_machine_url` as the machine prefill when present.
3. The system browser opens `{gateway}/oauth/authorize` as public client
   `tidebreak-mobile` with PKCE S256. The redirect is the app scheme plus
   `://callback` (`tidebreak://callback` in production).
4. The authorization code is exchanged at `{gateway}/oauth/token`. Refresh
   tokens rotate; only `control` and `tidebreak:<hex>` resources are minted.
5. When the gateway advertised a machine URL, the app attaches to it
   automatically and lands on the hub. There is no confirm step: attach
   validation refuses any machine but the paired deployment's own, so
   confirming a prefilled field decides nothing.

Attach validates the machine URL the same way desktop does, reads
`/auth/discovery`, derives `tidebreak:<sha256(canonical_url)>` locally, and
refuses a mismatched echo or a gateway URL that is not the paired deployment.
`GET /policy` is the authenticated probe. Auto-attach runs exactly this
sequence — nothing is skipped but the tap.

The Attach screen is the fallback, and it is where the app lands whenever
auto-attach cannot finish:

- the gateway advertised no `tidebreak_machine_url` (enter one),
- discovery timed out after 10s — usually a hosted machine on a VPN this phone
  isn't on, shown with that hint and a Retry,
- the echo or gateway URL failed validation,
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
