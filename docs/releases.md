# Releases and versioning

Tidebreak uses one Semantic Version for the desktop product. A published native
GitHub Release and its `vMAJOR.MINOR.PATCH` tag are the source of truth. There is
no release branch, release pull request, or committed version-bump commit.

The `0.0.0` values in Cargo, Tauri, and the private UI package are development
metadata. On a release build, the workflow validates the tag, passes its version
to Tauri through a configuration overlay, and exports `TIDEBREAK_VERSION` so Rust
code reports and gates on the same product version.

## Classify every pull request

Pull request titles use a Conventional Commits header:

```text
type(optional-scope)[!]: description
```

CI validates the title, and squash merge makes it the commit title on `main`.
Scopes are optional and descriptive; common examples are `core`, `server`,
`desktop`, `mcp`, `cli`, `deps`, and `release`.

| Title type                                          | Use for                                              | Release effect |
| --------------------------------------------------- | ---------------------------------------------------- | -------------- |
| `feat`                                              | New user-visible capability                          | Minor          |
| `fix`                                               | User-visible defect correction                       | Patch          |
| `perf`                                              | User-visible performance improvement                 | Patch          |
| `deps`                                              | Shipped runtime dependency update                    | Patch          |
| `revert`                                            | Reversal of a previously shipped change              | Patch          |
| `docs`, `refactor`, `test`, `build`, `ci`, `chore`  | Non-user-facing maintenance                          | None           |
| `feat`, `fix`, `perf`, `deps`, or `revert` with `!` | Breaking product, API, data, or configuration change | Breaking       |

Examples:

```text
feat(desktop): add document search
fix(core): prevent duplicate turn completion
deps(cargo): update the database stack
revert: restore the previous storage behavior
feat(core)!: replace the persisted conversation format
docs: explain local model configuration
```

Choose the type based on user impact, not the files changed. A refactor that
fixes observable behavior is `fix`; a build change that adds a shipped
capability is `feat`. A breaking refactor is normally `feat!` or `fix!`, based
on its user impact. CI rejects `!` on maintenance-only types. If the PR body has
a `BREAKING CHANGE:` footer, its title must also carry `!` so the impact is
visible during review.

GitHub Actions dependency updates remain `ci(deps)` and do not release the
product. Cargo Dependabot updates use `deps` because those dependencies ship in
the desktop app.

User-facing PRs also receive a managed release-note label derived from the same
validated title. These labels make the native release notes easier to scan
without asking authors to classify a PR twice:

| Title type | Release-notes section       |
| ---------- | --------------------------- |
| `feat`     | New Features                |
| `fix`      | Bug Fixes                   |
| `perf`     | Performance Improvements    |
| `deps`     | Dependency Updates          |
| `revert`   | Reverted Changes            |
| Any `!`    | Breaking Changes            |

Documentation and other maintenance-only types carry no `release-note:*` label
and appear in a trailing **Maintenance** section instead — listed, not dropped,
because a maintenance change can still matter to someone updating (a schema
baseline rebuild, a toolchain requirement). Every merged PR is accounted for in
the notes. In the rendered
notes, the section heading supplies the type, so the draft formatter removes the
redundant Conventional Commit prefix from each PR title. Maintenance entries
keep their full prefixes: that section mixes types, so the prefix is the
information. When a category has
multiple user-facing changes with the same scope, the formatter groups them
under a third-level heading: for example, multiple `feat(desktop)` changes are
rendered under **New Features**, then **Desktop**. A singleton scope is kept
compact as an inline prefix, and unscoped changes appear in the flat tail of the
section.

For historical or imported pull requests, the **Release draft** workflow has a
manual `workflow_dispatch` backfill. Its default dry run reports the exact
changes; rerun it with `apply` enabled to synchronize only the managed
`semver:*` and `release-note:*` labels from titles that pass the current policy.
It deliberately leaves free-form historical titles in **Other Changes** rather
than guessing their impact. The job uses the repository `GITHUB_TOKEN`, so its
label writes do not fan out into hundreds of labeled-event workflow runs.

## How the native release draft works

The release-draft workflow keeps exactly one draft GitHub Release up to date:

1. A trusted workflow maps the validated PR title to exactly one managed
   `semver:*` label and, for a non-breaking user-facing change, one managed
   `release-note:*` category label. Required CI verifies that exact label set
   before merge.
2. After the PR is squash-merged to `main`, Release Drafter adds it to the
   native draft, groups the release notes into the sections above, and suggests
   the next tag. The draft formatter then groups repeated scoped Conventional
   Commit titles beneath scope subheadings and keeps other entries compact at
   the end of their section. Breaking changes are always shown first.
3. Maintenance-only PRs land in the trailing **Maintenance** section, so every
   merged PR is accounted for in the notes. The largest release effect among
   all PRs chooses the proposed version; the category labels do not
   independently change the version, and a maintenance-only release proposes a
   patch bump.

The first release has no previous published release to use as a comparison
baseline, so Release Drafter intentionally leaves that draft as a manual
starting point. For `v0.1.0`, set the tag deliberately and click **Generate
release notes** in GitHub; `.github/release.yml` applies the same sections to
GitHub's native output. GitHub's native generator cannot create dynamic scope
subheadings, so add those manually if desired for that one-time full-history
release. Curate that result before publishing. From then on, the last published
tag and the managed PR labels make the maintained draft and its proposed version
automatic.

## Publishing a release

1. Open **Releases** in GitHub and select the existing draft.
2. Confirm the target is the intended commit on `main`, the proposed
   `vMAJOR.MINOR.PATCH` tag is correct, the release is not marked as a
   prerelease, and the notes contain the intended PRs.
   For the first release only, set `v0.1.0`, click **Generate release notes**,
   and curate the full-history result as described above.
3. Complete the release-readiness review, but do **not** publish the draft in
   GitHub. Immutable releases cannot accept assets after publication.
4. Open **Actions → Publish desktop release → Run workflow**, select `main`,
   and enter the draft's exact tag. The workflow queries that draft from the
   trusted `main` workflow, rejects malformed tags or prereleases, creates the
   tag at the current `main` commit when it does not already exist, and retains
   the same tag on a retry. It snapshots the draft metadata so a later merge
   cannot change the notes or proposed version while the build is running, and
   pins every later job to the frozen commit SHA.
5. In parallel with the desktop build, the documentation builder checks out
   that validated SHA and builds `docs-site/` as a static export under `/docs`.
   Publication waits until the GitHub Release itself has been published. It
   then creates an unaliased production-target deployment in the
   dedicated Vercel project, smoke-tests the staged root page, nested content,
   search index, sitemap, assets, and canonical metadata, and only then
   promotes that immutable deployment to `tidebreak-docs.vercel.app`. The
   `docs-production` environment supplies the only required secret,
   `VERCEL_TOKEN`; its project and organization identifiers are non-secret
   workflow constants. A failed build or smoke test leaves the previous docs
   deployment serving production.
6. The workflow first checks whether that exact tag, commit, and
   publication date already have a complete immutable release on S3.
   Credential-free prerequisites — one per platform — otherwise compile the tag
   with its product version and save unsigned Cargo outputs before any signing
   or notarization can fail.
7. The production jobs reuse those prepared compiler outputs — every cache key
   they can restore names the release commit or, at minimum, the `Cargo.lock`
   and toolchain hashes, and they delete the linked binaries the archive
   carries so the products they package are always linked from the tag's own
   sources. The macOS job then signs the app with the Developer ID identity,
   notarizes and staples the app and DMG, verifies them with Apple tooling, and
   creates a signed Tauri updater archive. The parallel Windows job is parked
   behind a literal `false` and never runs — see
   [What comes after v1](deferred.md) — so a release currently produces macOS
   artifacts only.
8. For a release that is not already hosted, a separate least-privilege job
   generates an SPDX JSON SBOM from the exact released source and checksums it
   independently of the package builds. That job has no production environment,
   deployment variables, OIDC permission, or AWS role; it transfers the two
   files to the publisher through a pinned GitHub artifact action. Publication
   waits for both the package build and source SBOM. The source-tree SBOM is
   deliberately published as source-scoped metadata, not attested as a
   description of the packaged installers.
9. Before publication, a credential-free job attaches the verified build
   outputs and their `.sha256` sidecars to the draft GitHub Release — today that
   is the notarized disk image as
   `Tidebreak-macos-universal.dmg`, plus the byte-identical legacy
   `Tidebreak-macos-apple-silicon.dmg` alias that keeps the existing README URL
   live through the transition. It retains the versioned app zip, signed updater
   archive, updater signature, source SBOM, and checksum sidecars on GitHub as
   recovery inputs. It would again include the Windows installer and updater
   assets if the Windows lane were unparked. This job holds no signing or AWS
   credentials. The two DMG names omit the version so that
   `https://github.com/brightwave-inc/tidebreak/releases/latest/download/<name>`
   stays a permanent download link for the README; the release page and the
   app's own version string identify which build it is.
10. Only after every GitHub asset is present does the workflow restore the
    frozen title and notes and publish the draft. GitHub then locks the release
    tag and assets. The workflow uses GitHub's resulting publication timestamp
    to create and attest the hosted manifest, uploads immutable versioned files,
    advances the public manifests, invalidates their CDN paths, and smoke-tests
    the hosted release. If a prior attempt already uploaded the complete
    immutable prefix, a new dispatch validates and reuses those bytes, skips the
    desktop build, and resumes only mutable metadata publication, CDN
    invalidation, and smoke testing. If GitHub publication succeeded but hosted
    publication did not, the retry downloads and verifies the retained GitHub
    assets, reconstructs the hosted release inputs from those immutable bytes,
    and resumes S3/CDN publication without rebuilding signed or notarized
    artifacts.

Running **Publish desktop release** is the only release operation. Merging
ordinary PRs updates the draft but never builds or ships a desktop version, and
manually clicking GitHub's **Publish release** button is no longer part of the
procedure. The GitHub Release becomes public only after its verified assets are
attached; the release is considered fully shipped when the workflow also
finishes hosted metadata and documentation publication successfully.

## Public desktop delivery

### One universal macOS application

A release ships one universal app, DMG, zip, and signed updater archive. Tauri
builds the desktop for both `aarch64-apple-darwin` and
`x86_64-apple-darwin` and combines the app executable. The before-build hook
builds the host broker for both targets and combines those slices with `lipo`
before bundling. Release verification rejects the bundle unless both the main
executable and the host broker contain both slices.

`RELEASE_PLATFORMS` in `scripts/create-release-manifests.mjs` is the single
source of truth for what a release contains: it drives the manifest, the
`latest.json` platform keys, and the immutable prefix below. The immutable
manifest contains one `macos/universal` artifact set; `latest.json` advertises
that same signed updater archive under both `darwin-aarch64` and
`darwin-x86_64`, so either native updater downloads identical universal bytes.

The preflight that resumes an already-hosted release validates it against the
current platform set. A release published before this platform change cannot
be re-dispatched: it fails on the artifact paths and updater keys instead of
silently republishing a different release shape.

### Windows: parked, unsigned x86_64 when it returns

**The Windows release lane is currently parked**, so releases are macOS-only.
`prepare_windows` and `build_windows` in `release.yml` are each gated behind a
literal `false`, and the `windows` descriptor in
`scripts/create-release-manifests.mjs` is retained outside `RELEASE_PLATFORMS`
rather than deleted. The reason is release time, not a product decision — see
[What comes after v1](deferred.md). The packaging below is what shipped through
v0.34.0 and what resuming the lane restores; nothing in it has been removed.

A release ships one `x86_64` Windows build as a single NSIS `-setup.exe`
installer. NSIS is the one installer format for v1 because Tauri bundles it
with no additional configuration and it installs per-user without elevation.
The installer is deliberately **not** Authenticode-signed yet, so Windows
SmartScreen will warn on first run; code signing is tracked separately and
must not be confused with the Tauri updater signature the release does carry.
That updater signature covers the exact installer bytes and feeds
`latest.json`'s `windows-x86_64` entry — Tauri v2 installs updates from the
installer itself, so no separate updater archive exists on Windows. Packaged
apps currently run the update loop only on macOS; the Windows metadata is
published so updater-enabled Windows builds can adopt it without a manifest
change. While the lane is parked, `latest.json` carries no `windows-x86_64`
key at all, so a Windows user on v0.34.0 has no upgrade path until it returns.

Unlike macOS, no cache-warming workflow exists for Windows: the credential-free
`prepare_windows` job compiles the tag from scratch (or from an earlier
release's prepared cache) and is the single writer of the Windows Cargo
registry and prepared-build caches.

The public download contract is rooted at:

```text
https://downloads.brightwave.io/tidebreak/
```

Each release has an immutable prefix:

```text
tidebreak/releases/vMAJOR.MINOR.PATCH/
├── manifest.json
├── macos/
│   └── aarch64/
└── windows/          (absent while the Windows lane is parked)
    └── x86_64/
```

Each macOS architecture directory contains a notarized DMG, a zip of the
notarized app, a signed `.app.tar.gz` updater archive, its signature, and
SHA-256 files. A Windows architecture directory, when the lane produces one,
contains an unsigned NSIS installer, its Tauri updater signature, and SHA-256
files. The immutable release root also contains
`Tidebreak_VERSION_source.spdx.json` and its checksum. The root `manifest.json`
inside each versioned prefix is immutable; only the unversioned
Tauri-compatible `manifest.json` and `latest.json` pointers are mutable. The
workflow refuses to overwrite a versioned object with different bytes or move
`latest.json` to an older version.

After the repository is public, verify the independently signed provenance for
any downloaded artifact with GitHub CLI:

```sh
gh attestation verify Tidebreak-macos-apple-silicon.dmg \
  --repo brightwave-inc/tidebreak
```

GitHub artifact attestations require a public repository unless the owner uses
GitHub Enterprise Cloud. While this repository remains private on another
plan, the workflow still publishes the checksummed, source-scoped SBOM but
skips provenance attestation rather than making private release retries fail.
The SBOM inventories the released source checkout; it must not be interpreted
as an inventory of files or dependencies embedded in the DMG, app bundle, or
updater archive.

Packaged macOS apps check `latest.json` 15 seconds after launch and every five
minutes. When a newer signed version is available, the Tauri updater downloads
and installs it in place, then emits a ready state to the UI. The user must
choose **Restart to update** before Tidebreak relaunches; the app never
interrupts active work automatically. Development builds do not contact an update feed. Packaged staging builds
check the staging feed under `/tidebreak/staging/latest.json` instead.

The first release containing this client integration is a bootstrap release:
older installed binaries have no updater and therefore cannot discover it.
Users must install that first updater-enabled release manually; subsequent
releases can advance automatically.

**Warm macOS release cache** runs independently after relevant changes land on
`main`. A short prerequisite job saves Cargo dependency downloads before the
build job compiles the desktop app with `--no-bundle`. This keeps a later UI
setup or compilation failure from also losing newly downloaded Cargo
dependencies. Each architecture then saves one unsigned archive containing
Cargo fingerprints, build-script outputs, compiled dependency files, and the
credential-free Rust products produced by `--no-bundle`: the desktop
executable, host broker, and desktop libraries. Keeping
those final unsigned products lets an exact-source build skip the otherwise
expensive desktop relink. The archive is saved before a failed compile is
reported, so successful partial work remains reusable. None of these jobs load
the `desktop-production` environment, sign, or publish. Because cache warming
has its own workflow and failure boundary, a later signing, notarization, or
publication failure cannot prevent that main-tip cache run from finishing. If
the shared cache is empty or has been evicted, manually run **Warm macOS release
cache** from `main`; the production release workflow also compiles the exact tag
and product version in a credential-free prerequisite. That prerequisite saves
its release-specific unsigned archive before reporting a compile failure. The
later `desktop-production` jobs are restore-only, so signing and notarization
can be retried without losing completed Rust work.

### Production environment configuration

Create a GitHub environment named `desktop-production`. Store these secrets in
that environment:

| Secret                               | Value                                                       |
| ------------------------------------ | ----------------------------------------------------------- |
| `APPLE_CERTIFICATE`                  | Base64-encoded Developer ID Application `.p12`              |
| `APPLE_CERTIFICATE_PASSWORD`         | Password used when exporting that `.p12`                    |
| `APPLE_API_PRIVATE_KEY`              | Complete App Store Connect `.p8` private key                |
| `TAURI_SIGNING_PRIVATE_KEY`          | Private key used to sign Tauri updater archives             |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Password for the Tauri updater-signing key                  |

Retain the Tauri updater keypair. Its public key is intentionally committed in
`crates/tidebreak-desktop/tauri.conf.json` so packaged apps can verify update
signatures. Only the private key and its password belong in GitHub secrets.

Configure these environment variables:

| Variable                                   | Value                                                            |
| ------------------------------------------ | ---------------------------------------------------------------- |
| `APPLE_SIGNING_IDENTITY`                   | Developer ID Application signing identity                        |
| `APPLE_API_KEY_ID`                         | App Store Connect API key ID                                      |
| `APPLE_API_ISSUER`                         | App Store Connect API issuer UUID                                 |
| `AWS_RELEASE_ROLE_ARN`                     | GitHub OIDC role allowed to publish Tidebreak release files        |
| `DOWNLOADS_S3_BUCKET`                      | S3 bucket behind `downloads.brightwave.io`                        |
| `DOWNLOADS_CLOUDFRONT_DISTRIBUTION_ID`     | CloudFront distribution serving the bucket                        |
| `DOWNLOADS_AWS_REGION`                     | AWS region; defaults to `us-east-1` when omitted                   |

The IAM role must trust GitHub's OIDC provider with the environment subject
`repo:brightwave-inc/tidebreak:environment:desktop-production`. Grant only the
S3 permissions needed beneath `tidebreak/` and CloudFront invalidation access for
the configured distribution. No long-lived AWS access key belongs in GitHub.

### Staging desktop from main

Every relevant push to `main` also publishes a packaged **staging** app, a
third desktop identity that can run beside both `cargo tauri dev` and an
installed release. The contract is recorded in
[decision record 16](decisions/0016-desktop-staging-channel.md).

Staging is a release-profile build with a blue icon, product name
`Tidebreak [staging]`, identifier `io.brightwave.tidebreak.staging`, keychain
service `tidebreak.staging`, and the `tidebreak-staging://` scheme. It does
not share a single-instance lock, app-data directory, updater feed, or
updater signing key with production. Its versions are
`0.0.0-staging.{run_number}` — monotonic for the Tauri updater, and not a
production `vMAJOR.MINOR.PATCH` tag.

The caller is **Publish staging desktop**. It derives the version, then
invokes the `workflow_call`-only **Publish staging desktop build** workflow
with `channel: staging`. Staging publishes serialize so an in-flight
notarization is not cancelled by the next merge; `latest.json` still only
advances if that commit is still `main`. Production's concurrency group is
untouched. Staging artifacts live under
`https://downloads.brightwave.io/tidebreak/staging/`; the publish step
refuses any other prefix and will not advance `latest.json` if `main` has
already moved on.

Create a GitHub environment named `desktop-staging`. Copy the Apple signing
secrets from `desktop-production`. Do **not** copy
`TAURI_SIGNING_PRIVATE_KEY` or `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`: a
stolen staging key must not verify on production clients. Staging has its
own updater keypair. The public half is committed in
`crates/tidebreak-desktop/tauri.staging.conf.json`. Set the staging
environment's `TAURI_SIGNING_PRIVATE_KEY` and
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` to that pair's private key and
password. Point `AWS_RELEASE_ROLE_ARN` at a
role whose GitHub OIDC subject is
`repo:brightwave-inc/tidebreak:environment:desktop-staging` and whose S3
write access is only `tidebreak/staging/*`. A role that can also write
`tidebreak/latest.json` would make a publish-guard bug a production incident.

Before the first public release, protect the environment as appropriate, verify
all configuration values, and exercise the workflow with the intended first
tag. The workflow references Apple signing secrets only in the macOS jobs; the
parked Windows jobs receive only the Tauri updater key, and publishing uses
short-lived AWS credentials obtained through OIDC.

### Release CI and cache security

Treat the release workflow as public even while the repository is private:

- Release actions are pinned to immutable commit SHAs. Dependabot is responsible
  for proposing reviewed action updates.
- An explicit manual dispatch on the protected `main` workflow freezes the
  native draft's tag before any production build. The build accepts only a
  non-prerelease release whose resolved commit is on `main`, and it publishes
  the GitHub Release only after attaching verified assets. It never runs with
  production secrets for a pull request or a manually selected feature branch.
- The jobs that build and sign check out that immutable commit SHA. The two
  AWS-credentialed jobs — the hosted-release inspection and the publisher —
  deliberately check out the dispatching `main` commit instead, so they run the
  release automation as it exists on `main` rather than as it existed at the
  tag. The consequence to keep in mind: manifests and hosted metadata are
  produced by main-tip `scripts/`, so a change to
  `scripts/create-release-manifests.mjs` alters what a *rerun* of an older tag
  would generate. The immutability preflight is what stops that from silently
  overwriting a published release — it requires an existing manifest to match
  before it can skip rebuilding, and a mismatch fails the run.
- Apple and Tauri credentials remain environment secrets. The Tauri private key
  reaches the configuration-validation precheck that runs before the build, and
  the post-notarization updater-signing and artifact-verification steps. It is
  not passed to the Tauri build action itself. AWS authentication uses GitHub
  OIDC, so no long-lived AWS key is stored in GitHub or the source tree.
  Infrastructure identifiers remain environment variables rather than committed
  configuration.
- The Developer ID certificate is imported into an ephemeral runner keychain
  before Tauri invokes the release-only resource-signing hook. The workflow
  verifies the configured identity is available, then deletes the keychain and
  decoded certificate even when the build fails.
- The dispatched builds run under the shared protected `main` cache scope, so
  later release tags can reuse earlier compiler outputs. A credential-free
  release prerequisite restores the main-tip archive, compiles the exact tag
  and version with `--no-bundle`, and saves a release-specific archive before
  reporting failure. It has no production environment or secrets. The
  secret-bearing jobs only restore that archive, before loading production
  secrets. Both cache writers include Cargo fingerprints, build-script outputs,
  dependency files, and explicitly named unsigned compiler products created
  without the production environment. They never include bundle directories,
  signed apps, DMGs, updater archives, signatures, keychains, or temporary
  Apple key files. `sccache` remains a read-only fallback because GitHub
  throttles its many small writes; the separate Cargo download cache retains
  `cache-targets: false`.
- The independent cache-warm workflow runs only for relevant pushes to `main`
  or an explicit manual dispatch. It has no production environment and receives
  no Apple, Tauri, or AWS credentials. It fetches Cargo dependencies early and
  stops after compiling with `--no-bundle`; it cannot sign or publish a
  release. Signing, notarization, or publication failures therefore do not
  cancel or roll back the separate main-tip cache run.
- Production artifacts are collected only after code-signing, notarization,
  stapling, and local verification succeed. The temporary App Store Connect key
  is removed even when the build fails.
- A retry never overwrites an immutable signed release. The preflight requires
  an existing manifest to match the requested version, tag, commit,
  publication date, filenames, URLs, sizes, and S3 digest metadata before it
  can skip rebuilding. The publisher then derives `latest.json` from that
  authoritative manifest and reruns metadata publication, CloudFront
  invalidation, and the complete hosted smoke test.
- Tauri notarizes and staples the app bundle. The workflow separately submits
  the signed DMG to Apple's notary service, requires an accepted result, and
  staples its ticket before artifact verification or upload.

Public source does not eliminate the need for operational controls. Restrict
who can publish releases and change Actions configuration, protect `main`, and
consider required reviewers on `desktop-production` before making the
repository public. Never add a pull-request trigger to the production workflow
or expose its environment secrets to code from forks.

### Third-party notices

Every shipped desktop artifact carries the licenses of the software it
redistributes. `legal/THIRD-PARTY-NOTICES.md` is generated from the resolved
Cargo workspace graph and the desktop UI's production npm graph by
`scripts/generate-third-party-notices.mjs`, and is checked in so a reviewer can
see exactly what a change to either lockfile adds to the product's obligations.

- Regenerate it with `node scripts/generate-third-party-notices.mjs` after any
  dependency change, from a checkout with UI dependencies installed. CI's
  `third-party notices` lane runs the same generator with `--check` and fails on
  drift, and the release build repeats that check before signing, so a tag can
  never ship notices that disagree with its lockfiles.
- The generator reads license facts from each package's own vendored files and
  manifest. `cargo metadata` and `pnpm licenses list` only enumerate the graphs
  and locate the packages, so neither tool's license classification can rewrite
  the notices. Declared expressions are reproduced verbatim, including compound
  ones; identical license texts are stored once and referenced by a
  content-addressed identifier.
- A package that declares no license is recorded as such rather than guessed
  at. Those entries are the ones to review: the notices are a compliance
  artifact, and an undeclared license in a distributed dependency is a question
  for a human, not something the generator should paper over.
- When that review settles a package's terms, the answer is recorded as a
  curated override in `CURATED_NODE_LICENSES` rather than left implicit. An
  override states the evidence it rests on, and the generator re-checks that
  evidence on every run: the package must still declare no license of its own,
  and its repository must still point where the review looked. Either check
  failing is a hard error, so a package that starts declaring a license, or
  whose repository moves, comes back for review instead of inheriting an old
  answer. Overrides quote license text from `scripts/license-texts/`, never
  from the network, so the output stays reproducible offline.
- Both graphs are host-independent today, so regenerating on macOS and checking
  on Linux agree. `cargo metadata` reports every package the lockfile can ship
  regardless of target, and no production UI dependency is platform-specific. If
  one ever is, pnpm will resolve a different closure per platform and the CI
  lane will disagree with a locally generated file — that disagreement is the
  signal to decide deliberately which platforms the notices must cover, not to
  regenerate until it passes.
- Tauri stages the file, along with `LICENSE` and `NOTICE`, into
  `Contents/Resources/legal/` of the app bundle. The DMG, the `.app.zip`, and
  the updater archive are all derived from that bundle, so the release lane
  verifies the bundled bytes match the checked-in files once, after signing.
  When the Windows lanes resume they inherit the same resource map.

## Before 1.0

While the latest published version is below `1.0.0`, fixes increment patch,
features increment minor, and breaking changes also increment minor. The
`semver:breaking` version-resolver entry in `.github/release-drafter.yml`
encodes that pre-1.0 behavior.

For example, `0.3.2` becomes `0.3.3` for a fix and `0.4.0` for either a feature
or a breaking pre-1.0 change.

## Preparing and shipping 1.0.0

`1.0.0` is a deliberate compatibility commitment. The current desktop schema
guard rejects every non-zero product major, so a `v1.0.0` app cannot initialize
its local profile until this checklist is complete:

1. Define the stable compatibility surface: persisted local data,
   configuration, CLI/API behavior, extension protocols, and supported upgrade
   window.
2. Replace the pre-v1-only guard in
   `crates/tidebreak-server/src/desktop_schema.rs` with the durable v1 lifecycle.
   It must preserve supported data, migrate transactionally, fail safely, and
   test upgrades from the latest 0.x state.
3. Verify the provisioned signed, notarized, hosted macOS pipeline with clean
   install and 0.x upgrade smoke tests. Add other supported platforms before
   claiming support for them.
4. Update `SECURITY.md` with supported release lines, security-fix policy, and
   end-of-support expectations. Document backup, migration, and rollback.
5. In the same readiness work, change the `semver:breaking` version resolver's
   `semver-increment` from `minor` to `major`. This activates normal SemVer for
   all releases after 1.0.
6. Finish the last 0.x release if needed, review the accumulated native draft,
   set its tag to exactly `v1.0.0`, and publish it.
7. Verify the tag, signed artifacts, hosted manifests, clean installation, 0.x
   upgrade behavior, and every reported application/protocol version.

After `1.0.0`, breaking changes increment major, features increment minor, and
fixes increment patch. Add supported release branches only if the project later
commits to maintaining multiple release lines.

## Required repository settings

Keep squash merge as the only merge method and set its defaults to **Pull
request title** and **Pull request body**.

Branch protection on `main` requires the individual CI jobs, not an aggregate
wrapper — there is none. The required contexts are `change scope`, `semantic PR
title`, `release policy`, `secret scan (gitleaks)`, `supply-chain advisories
(cargo-deny)`, `unused deps (cargo-machete)`, `third-party notices`, `rustfmt`,
`clippy`, `desktop test`, `test`, `postgres state machine`, and `desktop UI`,
each pinned to the GitHub Actions app (`app_id` 15368) so no other app can
satisfy them.
Every lane a change's scope can reach runs on the pull request itself; a lane
outside the scope reports a successful skip, which is what lets a required
check pass without running. Green PR checks are full platform-neutral
validation — see [`CLAUDE.md`](../CLAUDE.md) — re-backed by the same lanes on
every Rust-scoped push to `main` and on the weekly scheduled run. Keep the
whole set required so the skip-reporting stays wired up, and add any new
always-running lane to the list.

The `semantic version label` check from the release-draft workflow stays
non-required: the required `semantic PR title` job already fails unless the
managed release labels match the title, so requiring the label job too would
add nothing.

`Windows cargo check` is intentionally separate from the required contexts, and
is currently parked behind a literal `false` along with the release lanes — see
[What comes after v1](deferred.md) — so the `windows-ci` label does nothing
today. When it is unparked it runs automatically for Rust-scoped pushes to
`main`, weekly schedules, and manual dispatches, while pull requests opt in with
`windows-ci` when they touch a native Windows boundary. Keep it non-required so
pull requests do not wait for long-running native platform coverage; the
post-merge and scheduled runs remain the backstop.

The release-draft workflow uses the built-in `GITHUB_TOKEN`; it does not require
a personal access token.
