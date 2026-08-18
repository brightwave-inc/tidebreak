# 40. Ship x86_64 Windows and Linux desktop packages

- Status: Superseded by [decision 43](0043-ship-arm64-packages-and-enable-cross-platform-updates.md)
- Date: 2026-08-17
- Owners: desktop and release engineering
- Related: [`docs/releases.md`](../releases.md), [decision 16](0016-desktop-staging-channel.md)
- Supersedes: none

## Context

Tidebreak's production release currently publishes only a universal macOS
application. The Windows release implementation remains in the repository but
is disabled, and no Linux packaging implementation exists. This leaves users
on Windows and Linux to build from source even though the desktop shell, host
broker, deep-link handling, credential storage, and managed-policy paths already
have platform implementations.

The first resumed multi-platform release needs a package contract that the
release manifest, updater feed, GitHub assets, hosted immutable prefix, and
installation documentation can all enforce. It also needs to distinguish
shipping the desktop shell from claiming that every optional native capability
has reached macOS parity.

## Decision

Every new production desktop release ships these package sets together:

- one universal macOS build under `macos/universal`;
- one x86_64 Windows NSIS installer under `windows/x86_64`;
- one x86_64 Linux AppImage and one x86_64 Debian package under
  `linux/x86_64`.

The Windows installer and Linux packages are not operating-system code-signed
in this first slice. The Windows NSIS installer, Linux AppImage, and Debian
package are signed with the existing Tauri updater key so the release feed can
authenticate their exact bytes. Linux publishes distinct AppImage and Debian
updater targets because Tauri installs those package formats differently.

A fresh release is all-or-nothing across the three operating systems. It is not
published unless the macOS, Windows, and Linux package jobs all succeed and the
manifest generator sees every declared artifact and updater signature. Stable,
version-free GitHub asset aliases provide permanent download links, while the
hosted manifest continues to use versioned immutable paths.

This decision does not enable the in-app updater on Windows or Linux. It also
does not promise local native execution, managed Node or LibreOffice
installation, computer use, or code mode on those platforms where the existing
capability model reports them unavailable. Those are independent product
capabilities rather than packaging prerequisites.

## Alternatives Considered

### Continue publishing macOS only

Rejected because the desktop already has substantial Windows and Linux support,
and requiring every user on those systems to assemble a release toolchain is a
larger product gap than the remaining platform-specific optional features.

### Ship only a portable package on each platform

Rejected. NSIS provides the established Windows install and uninstall path.
On Linux, AppImage serves users who want a portable download while `.deb`
integrates with the most common supported desktop distribution family. Shipping
both avoids making one distribution preference the product boundary.

### Ship ARM64 packages in the same first release

Rejected for this slice. Windows and Linux ARM64 packaging would add another
native dependency and sidecar build boundary before the x86_64 packages have
completed clean-install and update rehearsal. The manifest shape can add an
architecture later without renaming the formats chosen here.

### Require Windows Authenticode signing before resuming releases

Rejected for this slice because Tidebreak has no accepted Windows signing
configuration in its release environment. Waiting for that infrastructure
would keep the existing installer path disabled. The unsigned status and
expected SmartScreen warning must remain explicit until signing is added.

### Enable automatic updates on all three operating systems immediately

Rejected because package publication and updater installation are separate
failure boundaries. Windows NSIS, Linux AppImage, and Linux Debian updates need
clean-machine rehearsal with the desktop's broker quiescence and restart
sequence before the runtime gate is widened. Publishing signed, format-specific
updater metadata now avoids another manifest migration when that validation is
complete.

## Consequences

Production release time and runner cost increase because Windows and Linux are
required outputs. A failure on either platform blocks the release rather than
quietly producing a partial platform set.

Windows users receive an unsigned installer and may see SmartScreen warnings.
Linux users choose between a portable AppImage and a Debian package; other
package ecosystems remain unsupported. Documentation must state the existing
platform capability gaps without describing the packages as incomplete or
experimental.

Published releases before this platform expansion do not satisfy the new
manifest contract and cannot be replayed through the current release workflow
as if they contained the new artifacts.

Revisit this decision when ARM64 clean-build coverage is available on both
platforms, when Windows signing infrastructure is accepted, when a Linux
package format materially broader than AppImage plus `.deb` is required, or
when updater installation rehearsal is sufficient to enable Windows or Linux
automatic updates.

## Validation

- Manifest tests require Windows and Linux artifacts, checksums, updater
  signatures, and `latest.json` platform keys.
- Workflow-policy tests require source-pinned Windows cache restoration,
  cache-write-free Linux packaging before updater signing, complete artifact
  transfer, and all-platform release gating.
- Native Windows CI checks the target graph, broker, SQLite profile lifecycle,
  and Credential Manager backend.
- The release jobs must produce exactly one NSIS installer, one AppImage, and
  one Debian package for x86_64 and must verify the packaged host-broker
  sidecar.
- Before the first public release, clean-machine smoke tests must cover install,
  launch, sidecar startup, persistence, deep links, and uninstall on Windows and
  Ubuntu, plus AppImage launch on a second Linux distribution.
