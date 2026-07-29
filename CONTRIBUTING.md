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

OpenWave keeps secrets in the login keychain, and macOS ties keychain approvals
to the binary's code signature — so unsigned dev builds would re-trigger the
access prompt on every rebuild. To prevent that, `cargo run` and `cargo test`
launch through
[`scripts/macos-dev-sign-runner.sh`](scripts/macos-dev-sign-runner.sh), which
signs the binary before running it.

**What makes an approval stick is a team identifier.** macOS records the
approval as a requirement it can re-evaluate — the code-signing identifier plus
the certificate's team — and any later build that satisfies it is let through.
A certificate without a team identifier gives it nothing stable to match, so
the approval is pinned to that one binary's cdhash and the next rebuild prompts
again. **Always Allow** cannot settle it either, and the partition-list repair
Apple documents also needs a team identifier.

So the runner signs with the first of these it finds:

1. `$OPENWAVE_DEV_SIGNING_IDENTITY`, if set — set it empty to opt out
2. an **Apple Development** identity
3. a **Developer ID Application** identity
4. an `openwave-dev` certificate already in a searchable keychain
5. a local-only `openwave-dev` identity it creates in a dedicated keychain
   under `~/Library/Application Support/OpenWave/dev-signing`

Options 2 and 3 carry a team identifier and are the ones that stop the prompts;
3 means local development signs with a distribution key, which is the price of
2 not being available. Option 5 needs no Apple account and unlocks with its own
generated password, so it never asks for your login-keychain password — but
being self-signed it has no team identifier, so credential prompts will keep
returning on rebuild. It is a floor, not a fix.

Every binary is signed with the same fixed identifier (`openwave-dev`), so one
approval covers every dev binary — including test executables, whose hashed
file names change between builds.

#### Credentials stored under a different identity

An item is bound to whatever signed the binary that **created** it. Credentials
stored before dev signing existed — or under an identity you have since moved
away from, including the self-signed fallback — keep prompting. Re-home them
once, which rewrites each item under the identity in use now:

```sh
cargo run -p openwave-cli -- rehome-secrets
```

Run it through Cargo so the signing runner applies. Each credential asks for
access once or twice more while it is read and its old item removed — plain
**Allow** is enough — and then stops, including after later rebuilds, provided
the identity carries a team identifier. `security find-generic-password -s
openwave.dev` lists what the dev profile has stored.

The first build after switching identities may also raise a one-time *codesign
wants to access key* prompt for the new signing key. **Always Allow** does
settle that one — `codesign` is a stable Apple-signed binary — or set the
key's partition list to avoid it up front.

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
