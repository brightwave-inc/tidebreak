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

## Apple-side prep, already done (web)

- Bundle id `inc.brightwave.tidebreak` registered under team
  `CUURNS78Y4`.
- App Store Connect app record created; numeric Apple ID `6810344419`
  (already in `eas.json`).

## Remaining one-time setup, in order

1. **(cli)** `pnpm dlx eas-cli@latest login` as a member of the
   `brightwave` Expo org. No Apple ID is needed for any step below: the
   Admin App Store Connect API key Tidewatch uploaded to EAS is
   team-scoped and is reused from EAS's credential store.
2. **(cli)** `eas init` in `mobile/` — creates the EAS project. eas-cli
   cannot write into a dynamic TS config, so paste the printed project
   id into `EAS_PROJECT_ID` in `app.config.ts`.
3. **(cli)** `APP_VARIANT=production eas credentials --platform ios` —
   choose App Store Connect API Key → use existing. EAS mints and
   stores the distribution certificate and provisioning profile;
   submits are forever non-interactive.
4. **(cli)** Skippable for a brand-new record (EAS starts at 1);
   mandatory if this bundle id ever uploaded before:
   `eas build:version:set --platform ios --profile production`.
5. **(cli)** `eas build --profile production --platform ios
   --auto-submit`.
6. **(web)** After the build reaches TestFlight (~5–15 min after
   processing): TestFlight → Internal Testing → create a group and add
   testers. Internal testers need no Beta App Review.

## Later, deliberately deferred

- **Android / Play Console** — package ids are already configured per
  variant; the Play record, service-account reuse, keystore, and the
  Console-UI-only first upload follow Tidewatch's DEPLOYING.md when a
  slice needs it.
- **The OTA + CI loop** — copy model-gateway's
  `.github/workflows/build-mobile.yml` (fingerprint routing: JS-only
  merges publish an OTA, native changes build + submit; PRs get a
  dry-run comment). Requires an org Expo access token stored as this
  repo's `EXPO_TOKEN` Actions secret:
  `gh secret set EXPO_TOKEN -R brightwave-inc/tidebreak`.
- **Per-variant icons** (tinted dev/staging) and a branded splash via
  the `expo-splash-screen` config plugin. The current icon is derived
  from the desktop tile; swap in a purpose-made 1024×1024 opaque source
  when design supplies one.

## Every release after that

Until the CI loop lands, releases are manual:

```sh
cd mobile
eas build --profile production --platform ios --auto-submit
```

JS-only changes can ship over the air instead:

```sh
eas update --channel production
```
