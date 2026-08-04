# Releases and versioning

OpenWave uses one Semantic Version for the desktop product. A published native
GitHub Release and its `vMAJOR.MINOR.PATCH` tag are the source of truth. There is
no release branch, release pull request, or committed version-bump commit.

The `0.0.0` values in Cargo, Tauri, and the private UI package are development
metadata. On a release build, the workflow validates the tag, passes its version
to Tauri through a configuration overlay, and exports `OPENWAVE_VERSION` so Rust
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

Documentation and other maintenance-only types remain excluded. In the rendered
notes, the section heading supplies the type, so the draft formatter removes the
redundant Conventional Commit prefix from each PR title. When a category has
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
3. Maintenance-only PRs are omitted. The largest release effect among included
   PRs chooses the proposed version; the category labels do not independently
   change the version.

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
3. Complete the release-readiness review, then click **Publish release**.
4. GitHub creates the tag and emits the `release.published` event. Confirm the
   draft still has its proposed `vMAJOR.MINOR.PATCH` tag before publishing:
   GitHub can otherwise publish an untagged release. A short-lived dispatcher
   rejects an invalid tag before it queues a production build; for a valid tag,
   it starts the production build from the current `main` workflow and passes
   only that tag. Running the build from `main` gives every release the same
   trusted compiler-cache scope; the build still queries GitHub for the
   published release, checks out that exact tag, rejects malformed tags,
   prereleases, drafts, or commits outside `main`, and pins all later jobs to
   the resolved commit SHA.
5. The dispatched workflow first checks whether that exact tag, commit, and
   publication date already have a complete immutable release on S3. A
   credential-free prerequisite otherwise compiles the tag with its product
   version and saves unsigned Cargo outputs before any signing or notarization
   can fail.
6. The production job restores those exact prepared outputs, signs the app with
   the Developer ID identity, notarizes and staples the app and DMG, verifies
   them with Apple tooling, and creates a signed Tauri updater archive.
7. Only after the build passes does the publisher upload immutable versioned
   files, advance the public manifests, invalidate their CDN paths, and
   smoke-test the hosted release. If a prior attempt already uploaded the
   complete immutable prefix, a new dispatch validates and reuses those bytes,
   skips the desktop build, and resumes only mutable metadata publication,
   CDN invalidation, and smoke testing.
8. A final job downloads the notarized disk image back from the CDN, checks it
   against the immutable manifest digests, and attaches it to the GitHub
   Release as `OpenWave-macos-apple-silicon.dmg` with a `.sha256` sidecar. It
   holds no signing or AWS credentials. The name omits the version so that
   `https://github.com/brightwave-inc/openwave/releases/latest/download/<name>`
   stays a permanent download link for the README; the release page and the
   app's own version string identify which build it is.

Publishing the native draft is the only release boundary. Merging ordinary PRs
updates the draft but never builds or ships a desktop version. A published
GitHub Release is considered shipped only when its dispatched **Publish macOS
release** build completes successfully; the initial dispatch run is not the
shipping signal.

## Public macOS delivery

### Apple Silicon only, for now

A release ships one `aarch64` build. Intel is paused while the product is in
active development. Cross-compiling `x86_64` on GitHub's arm64 macOS runners
takes roughly two and a half times as long as the native build for the
identical crate set — about 18 minutes against 7 on a recent release — so the
Intel job, not the one anybody installs, set the length of every release and
cache-warm run. No one on the team or in early testing uses an Intel Mac.

`MACOS_ARCHITECTURES` in `scripts/create-release-manifests.mjs` is the single
source of truth for what a release contains: it drives the manifest, the
`latest.json` platform keys, and the immutable prefix below. Restoring Intel
means adding `x86_64` there and adding its row back to the two `release.yml`
build matrices and the `cache-macos.yml` warm matrix.

Two consequences worth knowing. `latest.json` advertises only
`darwin-aarch64`, so an Intel install finds no update rather than a broken one.
And the preflight that resumes an already-hosted release validates it against
the current architecture set, so a release published before this change cannot
be re-dispatched; it fails on the artifact count instead of silently
republishing.

The public download contract is rooted at:

```text
https://downloads.brightwave.io/openwave/
```

Each release has an immutable prefix:

```text
openwave/releases/vMAJOR.MINOR.PATCH/
├── manifest.json
└── macos/
    └── aarch64/
```

Each architecture directory contains a notarized DMG, a zip of the notarized
app, a signed `.app.tar.gz` updater archive, its signature, and SHA-256 files.
The root `manifest.json` and Tauri-compatible `latest.json` are the only mutable
objects. The workflow refuses to overwrite a versioned object with different
bytes or move `latest.json` to an older version.

Packaged macOS apps check `latest.json` 15 seconds after launch and every five
minutes. When a newer signed version is available, the Tauri updater downloads
and installs it in place, then emits a ready state to the UI. The user must
choose **Restart to update** before OpenWave relaunches; the app never
interrupts active work automatically. Development and unsupported-platform
builds do not contact the production update feed.

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
`crates/openwave-desktop/tauri.conf.json` so packaged apps can verify update
signatures. Only the private key and its password belong in GitHub secrets.

Configure these environment variables:

| Variable                                   | Value                                                            |
| ------------------------------------------ | ---------------------------------------------------------------- |
| `APPLE_SIGNING_IDENTITY`                   | Developer ID Application signing identity                        |
| `APPLE_API_KEY_ID`                         | App Store Connect API key ID                                      |
| `APPLE_API_ISSUER`                         | App Store Connect API issuer UUID                                 |
| `AWS_RELEASE_ROLE_ARN`                     | GitHub OIDC role allowed to publish OpenWave release files        |
| `DOWNLOADS_S3_BUCKET`                      | S3 bucket behind `downloads.brightwave.io`                        |
| `DOWNLOADS_CLOUDFRONT_DISTRIBUTION_ID`     | CloudFront distribution serving the bucket                        |
| `DOWNLOADS_AWS_REGION`                     | AWS region; defaults to `us-east-1` when omitted                   |

The IAM role must trust GitHub's OIDC provider with the environment subject
`repo:brightwave-inc/openwave:environment:desktop-production`. Grant only the
S3 permissions needed beneath `openwave/` and CloudFront invalidation access for
the configured distribution. No long-lived AWS access key belongs in GitHub.

Before the first public release, protect the environment as appropriate, verify
all configuration values, and exercise the workflow with the intended first
tag. The workflow references Apple signing secrets only in the macOS jobs;
publishing uses short-lived AWS credentials obtained through OIDC.

### Release CI and cache security

Treat the release workflow as public even while the repository is private:

- Release actions are pinned to immutable commit SHAs. Dependabot is responsible
  for proposing reviewed action updates.
- A published native GitHub Release dispatches the production build on the
  protected `main` workflow. The build accepts only a published, non-prerelease
  tag whose resolved commit is on `main`. It never runs with production secrets
  for a pull request or a manually selected feature-branch workflow.
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
   `crates/openwave-server/src/desktop_schema.rs` with the durable v1 lifecycle.
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
title`, `release policy`, `secret scan (gitleaks)`, `rustfmt`, `clippy`,
`desktop test`, `test`, `postgres state machine`, and `desktop UI`. Only the
first five run on an ordinary pull request; the compile-heavy lanes (`clippy`,
`desktop test`, `test`, `postgres state machine`, `desktop UI`) are opt-in
behind the `full-ci` label and otherwise report a successful skip, which is what
lets a required check pass without running. That is the deliberate fast gate
described in [`CLAUDE.md`](../CLAUDE.md), backed by full validation on every
push to `main` and on the weekly scheduled run — not a claim that PostgreSQL
state-machine coverage gates every merge. Keep both sets required so the
skip-reporting stays wired up, and add any new always-running lane to the list.

The release-draft workflow uses the built-in `GITHUB_TOKEN`; it does not require
a personal access token.
