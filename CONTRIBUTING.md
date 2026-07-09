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
# Build everything
cargo build --workspace

# Formatting, lints, and tests (what CI runs)
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The Rust toolchain is pinned in [`rust-toolchain.toml`](rust-toolchain.toml);
`rustup` will pick it up automatically.

### Desktop UI

See [`crates/openwave-desktop/README.md`](crates/openwave-desktop/README.md).
Short version: `cd crates/openwave-desktop && pnpm --dir ui install && cargo tauri
dev`, or run the React UI in a browser against `openwave serve` via
`ui/.env.local`.

## Commit and PR conventions

- Use [Conventional Commits](https://www.conventionalcommits.org/) prefixes:
  `feat:`, `fix:`, `docs:`, `refactor:`, `chore:`, `build:`, `ci:`, `test:`.
- Write a clear PR description: what changed and why.

## Contributor License Agreement

By submitting a contribution, you agree that your contribution is provided under
the [Apache License 2.0](LICENSE) and that you have the right to submit it. A CLA
check runs on pull requests; you'll be prompted to sign once, and it applies to
all future contributions.

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). By
participating, you agree to uphold it.
