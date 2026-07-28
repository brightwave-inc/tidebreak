# Contributing to OpenWave

Thanks for your interest in OpenWave! It's early — the fastest way to help right
now is to try the walking skeleton as it lands, file sharp issues, and discuss
design before large changes.

## Ground rules

- **Open an issue before a large PR.** For anything beyond a small fix, let's
  align on the approach first so your time is well spent.
- **Keep changes focused.** One logical change per PR; small and reviewable beats
  big and sprawling.
- **No secrets, ever.** Never commit credentials, tokens, or `.env` files. CI
  runs a secret scan on every push and pull request.

## Development

```sh
# Run the desktop app: installs the UI dependencies, then opens the window.
# Arguments are forwarded to `cargo tauri dev`.
scripts/dev.sh

# Build everything
cargo build --workspace

# Formatting, lints, and tests (what CI runs)
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The Rust toolchain is pinned in [`rust-toolchain.toml`](rust-toolchain.toml);
`rustup` will pick it up automatically.

### macOS keychain prompts

OpenWave keeps secrets in the login keychain, and macOS ties keychain
approvals to the binary's code signature — so unsigned dev builds would
re-trigger the access prompt on every rebuild. To prevent that, `cargo run`
and `cargo test` launch through
[`scripts/macos-dev-sign-runner.sh`](scripts/macos-dev-sign-runner.sh), which
signs the binary with your `openwave-dev` or `Apple Development` certificate
(whichever exists) before running it. Every binary is signed with the same
fixed identifier, so clicking **Always Allow** once covers all dev binaries —
including test executables, whose hashed file names change between builds.
With no certificate in the keychain the wrapper is a no-op; you can also set
`OPENWAVE_DEV_SIGNING_IDENTITY` to pick an identity explicitly, or set it
empty to opt out.

If the prompt asks for your **login keychain password** (instead of a plain
Allow/Deny), the keychain item was created by an older ad-hoc-signed build
and its partition list doesn't include your signing certificate's team.
Repair it once per secret, then Always Allow sticks:

```sh
security set-generic-password-partition-list \
  -S "apple-tool:,apple:,teamid:<YOUR_TEAM_ID>" -s openwave -a <account>
```

(`security find-identity -v -p codesigning` shows the team id in parentheses;
`security find-generic-password -s openwave` lists the accounts.)

### Desktop UI

See [`crates/openwave-desktop/README.md`](crates/openwave-desktop/README.md).
Short version: `cd crates/openwave-desktop && pnpm --dir ui install && cargo tauri
dev`, or run the React UI in a browser against `openwave serve` via
`ui/.env.local`.

## Commit and PR conventions

- Pull request titles must use a
  [Conventional Commits](https://www.conventionalcommits.org/) header. CI checks
  the title because squash merge makes it the release commit on `main`:
  `type(optional-scope)[!]: description`.
- Allowed types are `feat`, `fix`, `perf`, `deps`, `revert`, `docs`, `refactor`,
  `chore`, `build`, `ci`, and `test`. Use `!` only with a release-driving type
  (`feat`, `fix`, `perf`, `deps`, or `revert`) and only for an intentionally
  breaking product, API, data, or configuration change.
- `feat` drives a minor release; `fix`, `perf`, shipped `deps`, and `revert`
  changes drive a patch; maintenance-only types do not release. Before 1.0,
  breaking changes drive a minor release. See the
  [release guide](docs/releases.md) for the full policy and the deliberate
  `1.0.0` procedure.
- Individual commits on a PR may use the same convention, but only the PR title
  becomes the squash commit used for release calculation.
- Write a clear PR description: what changed and why.

## Contributor License Agreement

By submitting a contribution, you agree that your contribution is provided under
the [Apache License 2.0](LICENSE) and that you have the right to submit it. A CLA
check runs on pull requests; you'll be prompted to sign once, and it applies to
all future contributions.

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). By
participating, you agree to uphold it.
