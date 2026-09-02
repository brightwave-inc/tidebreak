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

| Title type | Release-notes section          |
| ---------- | ------------------------------ |
| `feat`     | ✨ New Features                |
| `fix`      | 🐛 Bug Fixes                   |
| `perf`     | ⚡ Performance Improvements    |
| `deps`     | 📦 Dependency Updates          |
| `revert`   | ⏪ Reverted Changes            |
| Any `!`    | 💥 Breaking Changes            |

Documentation and other maintenance-only types carry no `release-note:*` label
and appear in a trailing **🧰 Maintenance** section instead — listed, not dropped,
because a maintenance change can still matter to someone updating (a schema
baseline rebuild, a toolchain requirement). Every merged PR is accounted for in
the notes. The rendered notes open with a thank-you. Category headings carry a
leading emoji so the sections scan quickly, and the draft does not wrap them in
a page-level heading; the release title is the tag. First-time contributors get
their own section, and the notes end with a compare link to the previous tag.
The section heading supplies the type, so the draft formatter removes the
redundant Conventional Commit prefix from each PR title. Maintenance entries
keep their full prefixes: that section mixes types, so the prefix is the
information. When a category has
multiple user-facing changes with the same scope, the formatter groups them
under a third-level heading: for example, multiple `feat(desktop)` changes are
rendered under **✨ New Features**, then **Desktop**. A singleton scope is kept
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
4. Before Release Drafter runs, the job lists GitHub Releases and `v*` tags
   (retrying a few times) and refuses to invoke it when version tags exist
   but no published release is visible. That is the outage that would
   otherwise mint a `v0.0.1` draft. If Release Drafter still loses the
   baseline after that check, the job deletes the fallback and fails so a
   later merge or a manual **Release draft** dispatch with **update-draft**
   can try again. Extra drafts are collapsed so publish still sees exactly
   one.

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
4. Open **Actions → Publish desktop release → Run workflow** and select
   `main`. Leave the tag blank to publish the current draft. Enter a tag only
   to retry that draft, an in-flight prerelease, or an already-published release.
   The workflow resolves a blank tag to the single non-prerelease draft, rejects
   a missing or ambiguous draft and malformed tags, creates the tag at the
   current `main` commit when it does not already exist, and retains the same
   tag on a retry. It snapshots the draft metadata so a later merge cannot
   change the notes or proposed version while the build is running, and pins
   every later job to the frozen commit SHA. Immediately after the snapshot, the
   draft is marked as a prerelease but stays a draft so assets can still be
   attached. Release Drafter only updates non-prerelease drafts, so later
   merges start a new notes draft instead of appending to the in-flight
   release. The GitHub Release is published only after those assets are
   attached.
5. In parallel with the desktop build, the documentation builder checks out
   that validated SHA and builds `docs-site/` as a static export under `/docs`.
   Publication waits until the GitHub Release itself has been published. It
   then creates an unaliased production-target deployment in the
   dedicated Vercel project, smoke-tests the staged root page, nested content,
   search index, sitemap, assets, and canonical metadata, and only then
   promotes that immutable deployment to `tidebreak-docs.vercel.app`. The
   `docs-production` environment supplies the only required secret,
   `VERCEL_TOKEN`, a team access token. Publication calls that send the token
   also send `teamId`. The project and organization identifiers are non-secret
   workflow constants. A failed build or smoke test leaves the previous docs
   deployment serving production.
6. The workflow first checks whether that exact tag, commit, and publication
   date already have a complete immutable release on S3. Credential-free macOS
   and Windows prerequisites otherwise compile the tag once with its product
   version. Each prerequisite uploads the final desktop binary, sidecars, and
   Tauri configuration in a run-scoped archive with SHA-256 manifests.
7. The macOS and Windows production jobs verify those archives before loading
   signing material, then run `tauri bundle` against the prepared binaries.
   They do not compile Rust or rebuild the frontend. The macOS job signs the app
   with the Developer ID identity, submits the DMG to Apple's notary service
   once, staples the resulting ticket to both the DMG and the app,
   verifies them with Apple tooling, and creates a signed Tauri updater archive.
   Parallel Windows and Linux jobs produce x86_64 and ARM64 NSIS, AppImage, and
   Debian packages; every package is signed with the Tauri updater key after
   packaging. A fresh release cannot continue unless every operating-system
   and architecture build succeeds.
8. For a release that is not already hosted, a separate least-privilege job
   generates an SPDX JSON SBOM from the exact released source and checksums it
   independently of the package builds. That job has no production environment,
   deployment variables, OIDC permission, or AWS role; it transfers the two
   files to the publisher through a pinned GitHub artifact action. Publication
   waits for both the package build and source SBOM. The source-tree SBOM is
   deliberately published as source-scoped metadata, not attested as a
   description of the packaged installers.
9. Before publication, a credential-free job attaches the verified build
   outputs and their `.sha256` sidecars to the draft GitHub Release. Stable
   download names include the notarized disk image as
   `Tidebreak-macos-universal.dmg`, plus the byte-identical legacy
   `Tidebreak-macos-apple-silicon.dmg` alias that keeps the existing README URL
   live through the transition, plus architecture-specific Windows installers,
   Linux AppImages, and Linux Debian packages. It also
   retains the versioned packages, updater artifacts and signatures, source
   SBOM, and checksum sidecars on GitHub as recovery inputs. This job holds no
   signing or AWS credentials. The stable names omit the version so that
   `https://github.com/brightwave-inc/tidebreak/releases/latest/download/<name>`
   stays a permanent download link for the README; the release page and the
   app's own version string identify which build it is.
10. Only after every GitHub asset is present does the workflow restore the
    frozen title and notes and publish the draft. GitHub then locks the release
    tag and assets. The workflow uses GitHub's resulting publication timestamp
    to create and attest the hosted manifest, uploads immutable versioned files,
    advances the public manifests, requests CDN invalidation of their paths
    without waiting for propagation, and smoke-tests
    the hosted release with cache-busting reads. If a prior attempt already uploaded the complete
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

### Windows: unsigned x86_64 and ARM64 NSIS

A release ships one Windows NSIS `-setup.exe` installer for each of `x86_64`
and `aarch64`. NSIS is the one installer format for v1 because Tauri bundles it
with no additional configuration and it installs per-user without elevation.
The installer is deliberately **not** Authenticode-signed yet, so Windows
SmartScreen will warn on first run; code signing is tracked separately and
must not be confused with the Tauri updater signature the release does carry.
Each updater signature covers the exact installer bytes and feeds the matching
`windows-x86_64` or `windows-aarch64` entry. Tauri v2 installs updates from the
installer itself, so no separate updater archive exists on Windows. Release
builds check that authenticated feed and ask before restarting into the new
installer.

Like macOS, Windows has a cache-warming workflow: **Warm Windows release
cache** compiles each architecture on pushes to `main` and saves the compiler
outputs, so the credential-free `prepare_windows` job usually restores a warm
archive instead of compiling the tag from scratch. That prepare job remains a
fallback writer of the Windows Cargo registry cache and saves the
release-specific prepared cache. The Windows ARM jobs keep the
`aarch64-pc-windows-msvc` Rust target and compile whisper.cpp with Ninja plus
`clang-cl`, because ggml refuses MSVC on ARM; the warm workflow uses the same
compiler setup so its CMake trees stay reusable in the release. After the
compile, the job transfers only the final binary, sidecars, and Tauri
configuration to the production job. The production job verifies and bundles
those files without installing a compiler or rebuilding the frontend.

Windows code mode uses the desktop's digest-verified managed Node ZIP and
pinned harness packages. Setup, archive, and quick-action commands run through
Windows PowerShell, while harness and command descendants are owned as one
process tree for interruption and timeout. Native local execution, managed
LibreOffice installation, and computer use remain separate platform
capabilities and are not implied by code-mode support.

### Linux: x86_64 and ARM64 AppImage and Debian packages

A release ships one portable AppImage and one `.deb` for each of `x86_64` and
`aarch64` Linux. Every package carries a Tauri updater signature over its exact
bytes, and `latest.json` publishes architecture- and format-specific entries so
an installed package can only select its own architecture and format. None of
the packages is distribution-signed in this shipping slice.

Release builds check the authenticated feed and ask before restarting. Tauri's
installed-bundle detection selects AppImage or Debian metadata before download.
Linux code mode uses the desktop's digest-verified managed Node runtime and
pinned harness packages. Native local execution, managed LibreOffice
installation, and computer use remain governed by their existing platform
capability checks; packaging the desktop does not claim those features on
Linux.

The Linux packaging job uses compiler and Cargo download caches in read-only
mode and does not enable pnpm caching. It restores the unsigned build archive
that **Warm Linux release cache** saves from `main`, discards any restored
product binaries so the shipped bytes are always produced by the release job,
and never saves a cache of its own. It builds both formats from the validated
release tag before the updater private key enters the step environment, then
signs and collects only the completed package bytes. This avoids exposing a
default-branch cache writer to code executed during a manually dispatched
release.

Hosted `ubuntu-22.04` runners pin apt at `azure.archive.ubuntu.com`, which can
stall on `apt-get update` for most of the 90-minute job. The packaging step
rewrites that mirror to `archive.ubuntu.com` (or `ports.ubuntu.com` on ARM),
sets short apt timeouts, and fails the step in eight minutes if the public
archive is also unreachable.

The public download contract is rooted at:

```text
https://downloads.brightwave.io/tidebreak/
```

Each release has an immutable prefix:

```text
tidebreak/releases/vMAJOR.MINOR.PATCH/
├── manifest.json
├── macos/
│   └── universal/
├── windows/
│   ├── x86_64/
│   └── aarch64/
└── linux/
    ├── x86_64/
    └── aarch64/
```

The macOS directory contains a notarized DMG, a zip of the notarized app, a
signed `.app.tar.gz` updater archive, its signature, and SHA-256 files. The
Windows directory contains an unsigned NSIS installer, its Tauri updater
signature, and SHA-256 files. The Linux directory contains an AppImage, a
Debian package, both updater signatures, and SHA-256 files. The immutable
release root also contains
`Tidebreak_VERSION_source.spdx.json` and its checksum. The root `manifest.json`
inside each versioned prefix is immutable; only the unversioned
Tauri-compatible `manifest.json` and `latest.json` pointers are mutable. The
workflow refuses to overwrite a versioned object with different bytes or move
`latest.json` to an older version.

After the repository is public, verify the independently signed provenance for
any downloaded artifact with GitHub CLI:

```sh
gh attestation verify Tidebreak-macos-universal.dmg \
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

**Warm macOS release cache**, **Warm Windows release cache**, and **Warm Linux
release cache** run independently after relevant changes land on `main`. In
each workflow a short prerequisite job saves Cargo dependency downloads before
the build job compiles the desktop app with `--no-bundle`. This keeps a later UI
setup or compilation failure from also losing newly downloaded Cargo
dependencies. Each architecture then saves one unsigned archive containing
Cargo fingerprints, build-script outputs, compiled dependency files, and the
credential-free Rust products produced by `--no-bundle`: the desktop
executable, host broker, and desktop libraries. Keeping
those final unsigned products lets an exact-source build skip the otherwise
expensive desktop relink. The archive is saved before a failed compile is
reported, so successful partial work remains reusable. None of these jobs load
the `desktop-production` environment, sign, or publish. Because cache warming
has its own workflows and failure boundary, a later signing, notarization, or
publication failure cannot prevent that main-tip cache run from finishing. If
a shared cache is empty or has been evicted, manually run the matching warm
workflow from `main`; the production release workflow also compiles the exact tag
and product version in a credential-free prerequisite. That prerequisite saves
its release-specific unsigned cache before reporting a compile failure. After a
successful compile, it uploads the final binary, universal sidecars, and Tauri
configuration in a one-day, run-scoped artifact. The `desktop-production` job
verifies that artifact before it loads signing material, then packages those
exact binaries without compiling again. Six warm archives plus the
release-specific ones compete for the repository's shared cache budget, so an
evicted archive costs a slower release, never a broken one.

To use a larger macOS runner for production compiles, set the repository
variable `PRODUCTION_MACOS_RUNNER` to a provisioned ARM64 organization runner
label. If you omit the variable, the workflow uses `macos-latest`. The signing
and notarization job stays on the standard runner because it no longer compiles
the application.

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

Tidebreak publishes a packaged **staging** app from `main`, a third desktop
identity that can run beside both `cargo tauri dev` and an installed
release. The contract is recorded in
[decision record 16](decisions/0016-desktop-staging-channel.md).

Staging is a release-profile build with a blue icon, product name
`Tidebreak [staging]`, identifier `io.brightwave.tidebreak.staging`, keychain
service `tidebreak.staging`, and the `tidebreak-staging://` scheme. It does
not share a single-instance lock, app-data directory, updater feed, or
updater signing key with production. Its versions are
`0.0.0-staging.{run_number}` — monotonic for the Tauri updater, not
contiguous, and not a production `vMAJOR.MINOR.PATCH` tag.

The caller is **Publish staging desktop**. It polls `main` hourly rather
than running on every push. A staging build takes about 45 minutes and every
build serializes on one publish group, so a per-push trigger could only queue
merges behind each other, and GitHub cancels the runs it cannot keep pending.
Each poll compares `main`'s tip against the commit recorded in the hosted
staging manifest and builds only when a staged path moved between them. To
build a commit the poll skips, run the workflow by hand with `force`.

The caller derives the version, then invokes the `workflow_call`-only
**Publish staging desktop build** workflow with `channel: staging`. Staging
publishes serialize so an in-flight notarization is not cancelled by the next
build. Production's concurrency group is untouched. Staging artifacts live
under `https://downloads.brightwave.io/tidebreak/staging/`; the publish step
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
tag. The workflow references Apple signing secrets only in the macOS jobs.
Windows and Linux receive only the Tauri updater key in their artifact
verification steps, and publishing uses short-lived AWS credentials obtained
through OIDC.

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
- The independent cache-warm workflows run only for relevant pushes to `main`
  or an explicit manual dispatch. They have no production environment and
  receive no Apple, Tauri, or AWS credentials. Each fetches Cargo dependencies
  early and stops after compiling with `--no-bundle`; none can sign or publish
  a release. Signing, notarization, or publication failures therefore do not
  cancel or roll back the separate main-tip cache runs.
- Production artifacts are collected only after code-signing, notarization,
  stapling, and local verification succeed. The temporary App Store Connect key
  is removed even when the build fails.
- A retry never overwrites an immutable signed release. The preflight requires
  an existing manifest to match the requested version, tag, commit,
  publication date, filenames, URLs, sizes, and S3 digest metadata before it
  can skip rebuilding. The publisher then derives `latest.json` from that
  authoritative manifest and reruns metadata publication, CloudFront
  invalidation, and the complete hosted smoke test.
- Notarization happens once per build: Tauri signs the app and DMG without
  notary credentials, then the workflow submits the signed DMG to Apple's
  notary service, requires an accepted result, and staples that one ticket to
  both the DMG and the identically signed app bundle before artifact
  verification or upload. The App Store Connect key reaches the job
  environment only after bundling.

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
  dependency change. The generator resolves the Cargo graph with every feature
  and installs the UI's production closure for every platform into a scratch
  directory, so the output is the same on any host: a package that ships one
  native build per platform is listed in full rather than as the variant the
  generating machine happens to run. CI's `third-party notices` lane runs the
  same generator with `--check` and fails on drift, and the release build
  repeats that check before signing, so a tag can never ship notices that
  disagree with its lockfiles.
- The generator reads license facts from each package's own vendored files and
  manifest. `cargo metadata` and the scratch `pnpm install` only resolve the
  graphs and put the packages on disk, so neither tool's license classification
  can rewrite the notices. Declared expressions are reproduced verbatim, including compound
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
  Windows and Linux packages inherit the same resource map; their packaging
  jobs verify the shared release inputs before producing artifacts.

## Before 1.0

While the latest published version is below `1.0.0`, fixes increment patch,
features increment minor, and breaking changes also increment minor. The
`semver:breaking` version-resolver entry in `.github/release-drafter.yml`
encodes that pre-1.0 behavior.

For example, `0.3.2` becomes `0.3.3` for a fix and `0.4.0` for either a feature
or a breaking pre-1.0 change.

Desktop upgrades from **v0.61.0** onward keep local data. Schema changes after
that pin are appended migrations
([decision 61](decisions/0061-schema-changes-are-migrations.md)). An upgrade
from **v0.60.0 or earlier** still rebuilds the SQLite profile once on first
open of a post-pin binary, because those databases predate the recorded
baseline. v0 will not reconstruct projects lost in that window. Hosted
PostgreSQL never used the epoch; it upgrades in place.

## Preparing and shipping 1.0.0

`1.0.0` is a deliberate compatibility commitment. The current desktop schema
guard rejects every non-zero product major, so a `v1.0.0` app cannot initialize
its local profile until this checklist is complete:

1. Define the stable compatibility surface: persisted local data,
   configuration, CLI/API behavior, extension protocols, and supported upgrade
   window.
2. Replace the product-major guard in
   `crates/tidebreak-server/src/desktop_schema.rs` with the v1 lifecycle. Most
   of what this item used to describe is already done:
   [decision 61](decisions/0061-schema-changes-are-migrations.md) froze the
   baseline, made every schema change an appended migration, and flipped the
   journal fixtures' failure messages back to "add an alias or write a
   migration". What is left is the release commitment itself — squash the chain
   into a single clean first migration for `1.0.0`, move `LAST_RESET_EPOCH` to
   that squash, and decide whether `reset_pre_v1_state` can finally go, which
   depends on whether any profile below the pin can still reach a v1 binary.
   The lifecycle must preserve supported data, migrate transactionally, fail
   safely, and test upgrades from the latest 0.x state.
3. Verify the provisioned release pipeline with clean install and 0.x upgrade
   smoke tests on macOS and both Windows architectures, clean install and
   update checks for both Linux Debian architectures, and AppImage launch and
   update checks on a second distribution.
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
`clippy`, `desktop test`, `Windows cargo check`, `test`, `postgres state
machine`, and `desktop UI`, each pinned to the GitHub Actions app (`app_id`
15368) so no other app can satisfy them.
Every lane a change's scope can reach runs on the pull request itself; a lane
outside the scope reports a successful skip, which is what lets a required
check pass without running. Green PR checks cover the scoped Linux lanes plus
Windows compilation for every Rust-scoped change, re-backed by the same lanes
on every Rust-scoped push to `main` and on the weekly scheduled run. Keep the
whole set required so the skip-reporting stays wired up, and add any new
always-running lane to the list.

The `semantic version label` check from the release-draft workflow stays
non-required: the required `semantic PR title` job already fails unless the
managed release labels match the title, so requiring the label job too would
add nothing.

`Windows cargo check` is rust-scoped like clippy because a Windows compile
break on `main` blocks the desktop release. Native Windows tests inside that
job still wait for the `windows` scope, the `windows-ci` label, or a
main/scheduled/manual run: those suites need NTFS and the Windows process
APIs, and they retry a PowerShell startup race that should not gate unrelated
Rust changes.

To run that job on an 8-core Windows larger runner (8 vCPU, 32 GB), set the
repository variable `CI_WINDOWS_RUNNER` to the provisioned x64 label. If you
omit the variable, the job uses `windows-latest`. Do not point the variable at
an ARM runner: the lane compiles `x86_64-pc-windows-msvc`.

The release-draft workflow uses the built-in `GITHUB_TOKEN`; it does not require
a personal access token.
