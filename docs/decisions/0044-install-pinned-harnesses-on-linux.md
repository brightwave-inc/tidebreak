# 44. Install pinned coding harnesses on Linux

- Status: Superseded by [decision 45](0045-run-code-mode-on-windows.md)
- Date: 2026-08-18
- Owners: code mode and desktop
- Related: [decision 41](0041-pinned-harness-binaries.md),
  [decision 43](0043-ship-arm64-packages-and-enable-cross-platform-updates.md),
  [`docs/code-mode.md`](../code-mode.md)
- Supersedes: [decision 41](0041-pinned-harness-binaries.md)

## Context

Decision 41 made Tidebreak install exact npm-package versions of each coding
harness under the app data directory instead of driving whatever happened to
be on the user's `PATH`. Its first implementation reused Tidebreak's managed
Node runtime, which only downloaded macOS artifacts because native local code
execution was the only consumer at the time.

Tidebreak now ships x86_64 and ARM64 Linux desktop packages. The Linux code-mode
runtime already has the required POSIX login-shell, git-worktree, checkpoint,
PTY, and harness-adapter paths, but it cannot install any pinned harness because
the managed Node prerequisite reports the platform unsupported. The same split
also leaves a latent macOS failure: npm-created harness entrypoints use
`#!/usr/bin/env node`, so probes and sessions must explicitly carry the managed
Node directory on `PATH` rather than assume the user installed another Node.

Windows is a different boundary. A Node artifact exists, but code mode still
has Unix-only setup and quick-action shell execution, no Windows login-shell
environment contract, and unproven process-tree interruption through npm's
Windows command shims. Treating the Node download as Windows code-mode support
would advertise a path whose lifecycle and worktree behavior are not yet
validated.

## Decision

The desktop-managed Node installer supports macOS and Linux on both x86_64 and
ARM64. Every target downloads the exact Node version already pinned by
Tidebreak, uses the architecture-specific official `tar.gz`, verifies its
published SHA-256 digest before unpacking, and records the same marker-gated
install under `tools/node/<version>/`.

The marker format, current-platform digest, and exact-directory resolver are a
shared execution contract rather than desktop-private logic. The desktop still
owns provisioning, but a headless server using the same Tidebreak data
directory may reuse that one verified install. Headless never scans sibling
version directories, trusts a marker for another artifact, downloads Node on
its own, or falls back to a system interpreter.

Pinned harness installation and execution on macOS and Linux always prepend
that verified managed Node `bin` directory to the captured child environment.
Version probes, authentication probes, model listing, and live sessions all use
the same environment, so a harness never succeeds only because an unrelated
system Node happens to be installed.

Linux therefore supports the existing pinned Claude Code, Codex CLI, opencode,
and Grok CLI code-mode path. This decision does not enable native local code
execution, LibreOffice conversion, or computer use on Linux; those remain
separate confinement and host-control boundaries.

Windows code mode remains unavailable. Its eventual implementation must cover
managed Node and harness artifacts together with Windows-native environment
capture, setup/quick-action command execution, process-tree cancellation, and
native worktree/session tests before the capability is advertised.

## Alternatives Considered

### Keep code mode macOS-only

Rejected for Linux. The existing code-mode runtime is already POSIX-shaped and
the missing managed Node artifact is a concrete, bounded prerequisite rather
than a new execution or approval model.

### Fall back to a user-installed Node or harness

Rejected. That recreates the version drift decision 41 removed, makes a GUI
launch depend on ambient `PATH`, and lets probes and sessions disagree about
which interpreter actually ran the pinned package.

### Enable Windows at the same time

Rejected for this slice. Downloading Node is necessary but not sufficient;
shipping Windows code mode without native lifecycle tests would turn several
known boundaries into user-visible failures.

### Bundle Node and every harness in the desktop package

Rejected. It multiplies package size across four desktop architecture builds
and ties harness pin bumps to the release archive, while the verified
data-directory install already provides an exact, replaceable runtime.

## Consequences

Linux first use can download the managed Node runtime and then the selected
harness packages. That costs disk and network time but keeps every harness
version deterministic and leaves the operating-system credential files under
the harness's own control.

The managed Node installer becomes a desktop capability independent of native
local code execution. Future changes must not re-couple its platform support to
the local sandbox gate.

Headless code mode remains a server/CLI surface under decision 7. It can reuse
an already provisioned managed Node runtime from its data directory, but the
Node installer stays in the closed native-only set; a fresh headless profile
reports the missing prerequisite rather than silently using ambient `PATH`.

Revisit the Windows exclusion when native tests prove repository registration,
worktree creation, setup and quick actions, harness probe/launch, interruption,
checkpointing, and restart recovery on Windows.

## Validation

- Platform-pin tests require shipped macOS/Linux architectures to select a
  managed Node artifact and unsupported targets to select none.
- Archive tests drive the real in-process `tar.gz` unpacker and require npm's
  symlink plus executable modes to survive.
- Harness tests require managed Node to be first on `PATH` for pinned probes,
  reject a pinned harness without a verified Node root, and pass that captured
  environment into the existing live-session launch paths.
- A headless-runtime test requires an existing marker-gated Node root and
  pinned harness to resolve without a desktop broker, while mismatched markers
  remain invisible.
- Linux workspace CI compiles and tests the installer and harness paths.
- Documentation states Linux code-mode support while continuing to name
  Windows code mode and the native execution/office/computer boundaries as
  unavailable.
