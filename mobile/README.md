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

## Pairing

1. Enter the gateway public base URL.
2. The app calls unauthenticated `GET /api/v1/meta` and stores
   `tidebreak_machine_url` as the machine prefill when present.
3. The system browser opens `{gateway}/oauth/authorize` as public client
   `tidebreak-mobile` with PKCE S256. The redirect is the app scheme plus
   `://callback` (`tidebreak://callback` in production).
4. The authorization code is exchanged at `{gateway}/oauth/token`. Refresh
   tokens rotate; only `control` and `tidebreak:<hex>` resources are minted.

Attach validates the machine URL the same way desktop does, reads
`/auth/discovery`, derives `tidebreak:<sha256(canonical_url)>` locally, and
refuses a mismatched echo or a gateway URL that is not the paired deployment.
`GET /policy` is the authenticated probe.

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
