# Deploying Tidebreak mobile

The pipeline is EAS Build + Submit: iOS under Apple team `CUURNS78Y4`,
TestFlight internal testing. It copies the practices proven in
model-gateway's mobile app (Tidewatch) — see `mobile/DEPLOYING.md` there
for the original rationale; divergences are noted inline here.

Legend: **(web)** = Apple web UI, **(cli)** = one-time interactive
terminal, **(repo)** = already done in this repository.

## Already in the repository (repo)

- `assets/icon.png` — 1024×1024, opaque, no alpha channel (an alpha
  channel is the classic silent TestFlight rejection). Derived from the
  desktop app icon; Expo derives every other iOS size at build time.
- `app.config.ts` — `icon`, `ios.appleTeamId`, export compliance
  pre-answered (`ITSAppUsesNonExemptEncryption: false`). Only
  secure-store/web-browser ship, so no `NS*UsageDescription` strings are
  owed yet; add one alongside any permission-touching module.
  **No `ios.buildNumber` anywhere** — the build number lives in EAS's
  remote counter. Variant selection is `APP_VARIANT`
  (production/staging/development), not Tidewatch's `APP_ENV`.
- `expo-updates` + `runtimeVersion: { policy: "fingerprint" }` — present
  from the very first binary so that binary already establishes the OTA
  fingerprint baseline (builds made without the dependency can never
  receive OTAs).
- `eas.json` — `appVersionSource: "remote"`; a `development` profile
  (dev client, internal) and one `production` shipping profile with the
  two lines everyone forgets: `autoIncrement: true` (without it every
  build reuses the same number and TestFlight rejects the second upload)
  and an explicit `environment`. `submit.production.ios.ascAppId` is the
  real record: `6810344419`.
- `.gitignore` — refuses `.p8`/`.p12`/keystores; EAS holds the real
  copies.

## Apple/Expo-side prep, already done (web/cli)

- Bundle id `inc.brightwave.tidebreak` registered under team
  `CUURNS78Y4`.
- App Store Connect app record created; numeric Apple ID `6810344419`
  (already in `eas.json`).
- EAS project `@brightwave/tidebreak-mobile` created via `eas init`;
  its id is hardcoded as `EAS_PROJECT_ID` in `app.config.ts` (eas-cli
  cannot write into a dynamic TS config).

## Remaining one-time setup, in order

1. **(cli)** `pnpm dlx eas-cli@latest login` as a member of the
   `brightwave` Expo org. No Apple ID is needed for any step below: the
   Admin App Store Connect API key Tidewatch uploaded to EAS is
   team-scoped and is reused from EAS's credential store.
2. **(cli)** `APP_VARIANT=production eas credentials --platform ios` —
   choose App Store Connect API Key → use existing. EAS mints and
   stores the distribution certificate and provisioning profile;
   submits are forever non-interactive.
3. **(cli)** Skippable for a brand-new record (EAS starts at 1);
   mandatory if this bundle id ever uploaded before:
   `eas build:version:set --platform ios --profile production`.
4. **(cli)** `eas build --profile production --platform ios
   --auto-submit`.
5. **(web)** After the build reaches TestFlight (~5–15 min after
   processing): TestFlight → Internal Testing → create a group and add
   testers. Internal testers need no Beta App Review.

## Later, deliberately deferred

- **Android / Play Console** — package ids are already configured per
  variant; the Play record, service-account reuse, keystore, and the
  Console-UI-only first upload follow Tidewatch's DEPLOYING.md when a
  slice needs it. Until then CI routes iOS only — see the next section.
- **Per-variant icons** (tinted dev/staging) and a branded splash via
  the `expo-splash-screen` config plugin. The current icon is derived
  from the desktop tile; swap in a purpose-made 1024×1024 opaque source
  when design supplies one.

## OTA + CI (the steady-state loop)

`.github/workflows/build-mobile.yml` runs on every push to `main` touching
`mobile/**` and routes by fingerprint, the same design as Tidewatch:

- The local `@expo/fingerprint` hash — computed with the project-resolved
  binary (`pnpm exec fingerprint`, which ships inside the pinned `expo`
  package), under the EAS `production` environment and
  `APP_VARIANT=production` — is compared against the last finished EAS
  build's `runtimeVersion`, per platform.
- **Hashes match** → `eas update` publishes an OTA to the `production`
  channel; installed clients pick it up on next launch. JS-only merges
  never touch the store.
- **Any mismatch** (native dep, config plugin, SDK bump — or no prior
  build) → `eas build --auto-submit` ships a binary to TestFlight.
- PRs that touch `mobile/**` get a dry-run: a PR comment says which of the
  two paths merging will take, with the per-platform hash table. Nothing is
  published or built from a PR. PRs that touch nothing under `mobile/**`
  skip the `deploy` job entirely, so it is safe to make a required status
  check on `main` — a skipped job satisfies one.
- Dependabot PRs also skip `deploy`. Dependabot-triggered runs read the
  Dependabot secrets store and never see `secrets.EXPO_TOKEN`, so the first
  EAS call would fail auth on a token Dependabot cannot be granted.

Routing is currently **iOS-only**, and that restriction is on routing, not
just on submission: with no finished Android build in EAS there is no
`runtimeVersion` for an `android` row to match, so including it would miss
on every run and pin the mode to `build` forever — the OTA path would never
fire. `eas update` still publishes both platforms' bundles. The workflow's
routing loop is already per-platform; enabling Android once the Play
Console setup above lands is a one-line change of the `PLATFORM` default
from `ios` to `all`.

Diverging from Tidewatch: the variant env var is `APP_VARIANT`, not
`APP_ENV`; there is no root `.nvmrc` here, so Node and pnpm are set up the
way `.github/workflows/mobile-checks.yml` does it (pnpm from
`mobile/package.json`'s `packageManager` pin, Node pinned explicitly).
Keep the two mobile workflows in step when either changes.

Fingerprint discipline: nothing non-deterministic in `app.config.ts` — a
value that changes between runs makes every push look like a native change
and the OTA path never fires.

One-time CI prerequisite (the workflow cannot create it, and no run will
get past its first EAS call without it):

```sh
gh secret set EXPO_TOKEN -R brightwave-inc/tidebreak
```

The value is an Expo access token (expo.dev → Access tokens; a robot token
survives personnel changes). Tokens are org-scoped, so Tidewatch's token
value works here — but the GitHub secret itself does not carry across
repositories and must be set on this one.

Expect the very first post-merge run to route to `build`, not `ota`: until
one binary exists there is no `runtimeVersion` to match against.

## Every release after that

Merge to `main`. CI routes OTA vs binary automatically. For a manual
binary outside CI:

```sh
cd mobile
eas build --profile production --platform ios --auto-submit
```

Or a manual OTA:

```sh
cd mobile
APP_VARIANT=production eas update --branch production --auto
```
