# 16. Desktop Staging Channel from Main

- Status: Accepted (amended 2026-08-21, see [Amendment](#amendment-2026-08-21))
- Date: 2026-08-13
- Owners: desktop and release
- Related: [releases](../releases.md), [decision 15](0015-tidebreak-product-and-technical-identity.md)
- Supersedes: none

## Context

Tidebreak already ships two desktop identities. A debug build is `Tidebreak
[dev]`: red icons, bundle id `io.brightwave.tidebreak.dev`, keychain service
`tidebreak.dev`, and the `tidebreak-dev://` scheme. An installed release is
`Tidebreak`: black icons, `io.brightwave.tidebreak`, the default keychain, and
`tidebreak://`. The split exists so `cargo tauri dev` can run beside a packaged
app without sharing a single-instance lock, app-data directory, or secrets.

There is no packaged build of current `main`. The only signed artifact comes
from a published GitHub Release. Testers who want tomorrow's code either run
debug locally or install a release that is already behind `main`. Those are
different apps with different data, so "try this before we ship" is not a
channel, it is a rebuild.

A third identity has to settle four things later work will live with: how the
app is recognized on the machine, how its versions compare, where it is hosted,
and how two notarizations racing down `main` interact with production.

## Decision

Tidebreak has three desktop channels. Staging is a packaged release-profile
build of `main`, not a debug build and not a GitHub Release.

| Channel    | Product name          | Identifier                          | Icon  | Keychain            | Scheme               | When                         |
| ---------- | --------------------- | ----------------------------------- | ----- | ------------------- | -------------------- | ---------------------------- |
| Production | Tidebreak             | `io.brightwave.tidebreak`           | black | `tidebreak`         | `tidebreak`          | published GitHub Release     |
| Dev        | Tidebreak [dev]       | `io.brightwave.tidebreak.dev`       | red   | `tidebreak.dev`     | `tidebreak-dev`      | debug / `cargo tauri dev`    |
| Staging    | Tidebreak [staging]   | `io.brightwave.tidebreak.staging`   | blue  | `tidebreak.staging` | `tidebreak-staging`  | relevant push to `main`      |

The three channels do not share a single-instance lock, app-data directory,
keychain service, deep-link scheme, updater feed, or updater signing key.
Staging therefore runs beside both a debug window and an installed release.
A staging binary answers
only `tidebreak-staging://`; it does not register or honor `tidebreak://`, so
it cannot steal production pairing links the way an early debug build used to.

Staging versions are SemVer prereleases of the reserved development version:

```text
0.0.0-staging.{github.run_number}
```

`run_number` is monotonic per the staging workflow, so the Tauri updater can
compare two staging builds. The version is not a production tag: `v0.0.0` is
already rejected as a release, and a prerelease cannot be mistaken for
`vMAJOR.MINOR.PATCH`. The hosted manifest records the full commit SHA. The
schema gate reads the leading `0`, so staging stays on the pre-v1 desktop
lifecycle.

Staging is hosted under its own prefix and feed:

```text
https://downloads.brightwave.io/tidebreak/staging/latest.json
https://downloads.brightwave.io/tidebreak/staging/releases/v0.0.0-staging.N/
```

Production `latest.json` and `tidebreak/releases/` are unreachable from this
channel. The GitHub environment is `desktop-staging`, with an IAM role scoped
to `tidebreak/staging/`. Apple signing material may be copied into that
environment. The updater keypair is not shared with production: staging
commits its own `plugins.updater.pubkey` in `tauri.staging.conf.json`, and
`desktop-staging` holds a matching `TAURI_SIGNING_PRIVATE_KEY` that must
never be copied from `desktop-production`. A stolen staging key must not
verify on production clients.

Concurrency:

- The staging caller uses a per-run group so a later merge cannot cancel it.
  The publish workflow serializes on `tidebreak-desktop-staging-build` with
  `cancel-in-progress: false`, so an in-flight notarization finishes.
- That run invokes a `workflow_call`-only staging publish workflow with
  `channel: staging`. The called run uses its own staging group so it cannot
  share or cancel `tidebreak-desktop-production-release`.
- At publish time, staging refuses any S3 key outside `tidebreak/staging/` and
  does not advance `latest.json` if `origin/main` has moved on.

The staging workflow itself does not mention `secrets.*`. It passes
`secrets: inherit` into the already-allowed release workflow. Production jobs
stay gated on a published tag or a production `workflow_dispatch`; they do not
run for `channel: staging`.

## Alternatives Considered

### Publish staging from the same identity as production

One bundle id and one data directory would let testers "just update." It was
rejected because a staging build of `main` can reset the pre-v1 schema epoch,
and sharing secrets or conversations with the installed release would make
that reset a data-loss event. The red/black split already exists to prevent
exactly that collision.

### Use the next draft version, `0.36.0-staging.N`

Anchoring to Release Drafter's proposed version would make About read like a
preview of the next ship. It was rejected because the draft version moves
when a `feat` lands, so two consecutive `main` tips can jump from
`0.36.0-staging.12` to `0.37.0-staging.13` for reasons unrelated to
installability, and a published `0.36.0` is greater than every
`0.36.0-staging.N`. Staging does not share a feed with production, so that
comparison is unused complexity. `0.0.0-staging.N` is boring and monotonic.

### Put the commit count or SHA in the version

A SHA is not monotonic, so the updater cannot decide that a later `main` tip
is newer. A commit count is monotonic until a force-push or a shallow clone
disagrees with GitHub. `run_number` is assigned by the workflow that publishes
the feed and does not depend on git history shape.

### A separate unsigned nightly

Skipping Developer ID and notarization would make the first install a
Gatekeeper fight and prevent the same updater path production uses. Staging
exists to exercise that path against `main`. The cost is Apple credentials in
a second environment.

### Fold staging jobs into every production trigger

One workflow file already holds the signing and publish machinery. Giving it a
`push: main` trigger without a separate caller would put staging and
production in the same concurrency group unless the group expression is
perfect, and a yellow `Publish desktop release` run on every merge would make
the production signal harder to see. A thin caller workflow keeps the
production trigger list intact.

### Share the production updater signing key

Isolating the feed URL and bundle id was thought to be enough, so staging
would sign with production's committed public key. Rejected: a leaked
`desktop-staging` private key would then verify on production clients. The
feeds being separate does not stop a client from accepting a signature its
embedded public key trusts.

### Do nothing

Testers would keep using debug builds or published releases. That leaves `main`
without a signed, auto-updating install and keeps the red/black pair as the
only way to tell two Tidebreaks apart.

## Consequences

Operators must create a `desktop-staging` GitHub environment before the first
staging publish, copy the Apple signing secrets into it, generate a distinct
Tauri updater keypair, commit only the public key in
`tauri.staging.conf.json`, and store that pair's private key and password in
the staging environment. Copying `TAURI_SIGNING_PRIVATE_KEY` from
`desktop-production` is forbidden. Grant its AWS role only
`tidebreak/staging/*` plus CloudFront invalidation for those paths. A role
that can also write `tidebreak/latest.json` would make a publish-guard bug a
production incident.

Staging installs are disposable relative to production. They will pick up
pre-v1 schema epoch resets from `main` and rebuild their local profile. That
is acceptable because the data directory is not the production one.

The updater on a staging install follows staging `latest.json` only. It will
never offer a production `vX.Y.Z`, and a production install will never offer a
staging prerelease.

Revisit this if production and staging need to share user data, if Apple
rejects `0.0.0-staging.N` as a `CFBundleShortVersionString`, or if a second
release line (LTS, beta) needs a fourth identity.

## Validation

- A staging binary reports identifier `io.brightwave.tidebreak.staging`,
  product name `Tidebreak [staging]`, and a version matching
  `0.0.0-staging.{n}`. A production binary still reports `io.brightwave.tidebreak`.
- Staging, debug, and production can be launched together; each keeps its own
  data directory and keychain service.
- `create-release-manifests` refuses a staging version under the production
  base URL, and refuses a production version under the staging base URL.
- The staging publish step refuses any S3 key that does not start with
  `tidebreak/staging/`.
- Staging overlay `plugins.updater.pubkey` exists and is not equal to
  production's committed updater public key.
- The staging caller workflow contains `secrets: inherit` and does not contain
  `secrets.`.
- A newer staging version may replace `latest.json`; an older one may not.
- Deep-link parsing in a staging build accepts `tidebreak-staging://` and
  refuses `tidebreak://`. Production still refuses both extra schemes.

## Amendment (2026-08-21)

**Staging builds from a poll of `main`'s tip, not from every relevant push.**
The "When" column for staging above should read that way; the rest of this
record stands unchanged.

A staging build takes about 45 minutes, and every build serializes on
`tidebreak-desktop-staging-build`. A per-push trigger could therefore only
queue merges behind each other, and GitHub keeps at most one pending run per
concurrency group: on a busy day each merge evicted the run queued before it.
Over one 25-commit stretch, 20 commits carried a cancelled staging check that
had never run a step, and GitHub rolls a cancelled check up as a red X, so
`main`'s commit list read as broken when it was not.

**Publish staging desktop** now runs on an hourly `schedule` and on
`workflow_dispatch`. Each poll reads the commit recorded in the hosted staging
manifest, compares it against `main`'s tip over the path list the push trigger
used to filter on, and builds only when one of those paths moved. A manual run
takes a `force` input to build a tip the poll would skip.

The concurrency rules above are unchanged: the caller still uses a per-run
group, and the publish workflow still serializes with
`cancel-in-progress: false` so an in-flight notarization finishes.
