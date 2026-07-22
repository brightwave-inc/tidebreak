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
`desktop`, `retrieval`, `mcp`, `cli`, `deps`, and `release`.

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

## How the native release draft works

The release-draft workflow keeps exactly one draft GitHub Release up to date:

1. A trusted workflow maps the validated PR title to one managed label:
   `semver:breaking`, `semver:minor`, `semver:patch`, or `semver:none`.
   Required CI also verifies that exact label before merge.
2. After the PR is squash-merged to `main`, Release Drafter adds it to the
   native draft, groups the release notes, and suggests the next tag.
3. Maintenance-only PRs are omitted. The largest release effect among included
   PRs chooses the proposed version.

The first release predates these managed labels, so set its tag deliberately
(normally `v0.1.0`) immediately before publishing. From then on, the last
published tag and the managed PR labels make the draft version automatic.

## Publishing a release

1. Open **Releases** in GitHub and select the existing draft.
2. Confirm the target is the intended commit on `main`, the proposed
   `vMAJOR.MINOR.PATCH` tag is correct, the release is not marked as a
   prerelease, and the notes contain the intended PRs.
3. Complete the release-readiness review, then click **Publish release**.
4. GitHub creates the tag and emits the `release.published` event. The macOS
   release workflow checks out that exact tag, rejects malformed tags or commits
   outside `main`, and derives the product version from the tag.
5. The workflow builds Apple Silicon and Intel apps, signs them with the
   production Developer ID identity, notarizes and staples the app and DMG,
   verifies them with Apple tooling, and creates signed Tauri updater archives.
6. Only after both architectures pass does the publisher upload immutable
   versioned files, advance the public manifests, invalidate their CDN paths,
   and smoke-test the hosted release.

Publishing the native draft is the only release boundary. Merging ordinary PRs
updates the draft but never builds or ships a desktop version. A published
GitHub Release is considered shipped only when its **Publish macOS release**
workflow completes successfully.

## Public macOS delivery

The public download contract is rooted at:

```text
https://downloads.brightwave.io/openwave/
```

Each release has an immutable prefix:

```text
openwave/releases/vMAJOR.MINOR.PATCH/
├── manifest.json
└── macos/
    ├── aarch64/
    └── x86_64/
```

Each architecture directory contains a notarized DMG, a zip of the notarized
app, a signed `.app.tar.gz` updater archive, its signature, and SHA-256 files.
The root `manifest.json` and Tauri-compatible `latest.json` are the only mutable
objects. The workflow refuses to overwrite a versioned object with different
bytes or move `latest.json` to an older version.

The current app does not yet install updates automatically; `latest.json` is the
stable server-side updater contract for that client integration.

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

Generate a dedicated Tauri updater keypair and retain its public key for the
future updater client configuration. Only the private key and its password
belong in GitHub secrets.

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

## Before 1.0

While the latest published version is below `1.0.0`, fixes increment patch,
features increment minor, and breaking changes also increment minor. The
`Breaking Changes` category in `.github/release-drafter.yml` encodes that
pre-1.0 behavior.

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
5. In the same readiness work, change the `Breaking Changes` category's
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
request title** and **Pull request body**. Keep the protected aggregate CI and
secret-scan checks required on `main`. The release-draft workflow uses the
built-in `GITHUB_TOKEN`; it does not require a personal access token.
