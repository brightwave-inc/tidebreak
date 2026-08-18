# 41. Code Mode Drives Pinned Harness Binaries

- Status: Superseded by [decision 44](0044-install-pinned-harnesses-on-linux.md)
- Date: 2026-08-17
- Owners: code mode
- Related: [`0034-harness-discovery-credentials.md`](0034-harness-discovery-credentials.md),
  [`0031-harness-adapter-boundary.md`](0031-harness-adapter-boundary.md)
- Supersedes: the "No bundling in v1" clause of
  [`0034`](0034-harness-discovery-credentials.md)

## Context

[`0034`](0034-harness-discovery-credentials.md) resolved harnesses through the
user's login shell so a GUI process could see the same `claude` the terminal
does. Capability flags were then gated to the one captured version. That
combination failed in the product: a user on Claude Code 2.1.234 saw only
Plan, because Ask, Auto, and Allow were marked unknown off 2.1.233.

The recorded escape hatch was the managed Node install: an exact version,
digest-checked, under the app data directory. Version drift is no longer
hypothetical. It is the reason a shipped mode list lies.

Credential observation still holds. A Tidebreak-owned binary reads the same
user home and config files the user's own install would. Pinning the
executable does not make Tidebreak a credential broker.

## Decision

**Tidebreak installs and launches a pinned copy of each harness.** The pin
is an exact npm package version, stored under
`{data_dir}/tools/harnesses/{kind}/{version}/`, with an `installed.json`
marker. Probe and launch use that binary. The user's `PATH` is not the
engine.

**The user's shell is still probed for environment.** Gateway config, `HOME`,
and other profile variables remain the child's environment. Only the
executable path is Tidebreak's.

**Credentials stay observed, never brokered.** A managed `claude` still
signs in through that product's own files and login flow.

**Capability flags describe the pin.** A probe of the managed binary reports
the modes that pin was captured against. Nearby patch versions of the same
minor line may keep those flags when the mapping is documented; a new
major/minor requires a new pin and a new capture.

**Doctor refresh installs a missing pin.** Create-session does the same if
the doctor has not run yet. A failed install is a visible doctor error, not
a silent fall-through to whatever is on `PATH`.

Deliberately excluded: pinning Windows builds (parked with the rest of
Windows), Tidebreak-mediated sign-in, and treating "latest" as a floating
tag at runtime. The pin is whatever was current when it was written; bump
it deliberately.

## Alternatives Considered

**Keep PATH discovery and loosen the version gate.** Fixes the 2.1.234 Plan
bug, but every new patch can change flags, and the product is back to
chasing user installs.

**PATH fallback when the pin is missing.** Rejected: the first session would
silently drive an unpinned binary and recreate the original bug.

**Bundle binaries in the app archive.** Rejected for size and update cadence.
The data-directory install matches Node and can be replaced when the pin
moves.

## Consequences

First code-mode visit may download four CLIs. Doctor must show install
progress and failure. A pin bump is a deliberate change, same as a Node
bump.

Revisit if a harness stops publishing an npm package we can pin, or if a
managed deployment needs the binary image baked in.

## Validation

- Resolution prefers `{data_dir}/tools/harnesses/...` over `PATH` when both
  exist.
- A probe with no data directory (tests) still uses the login-shell shim.
- A marker for the wrong version hides the tree.
- Claude 2.1.234 reports Ask, Auto, and Allow as supported.
