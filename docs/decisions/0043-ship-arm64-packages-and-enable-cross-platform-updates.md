# 43. Ship ARM64 packages and enable cross-platform updates

- Status: Proposed
- Date: 2026-08-18
- Owners: desktop and release engineering
- Related: [`docs/releases.md`](../releases.md), [decision 16](0016-desktop-staging-channel.md)
- Supersedes: [decision 40](0040-ship-x86-64-windows-and-linux-desktop-packages.md)

## Context

Decision 40 resumed Windows and Linux distribution with x86_64 packages while
deliberately parking ARM64 packages and updater installation outside macOS.
The production workflow now already creates signed NSIS, AppImage, and Debian
updater payloads and publishes format-specific feed entries. The remaining
boundaries are native ARM64 package builds and the runtime gate that prevents
Windows and Linux release builds from checking or installing those feeds.

GitHub-hosted native ARM64 runners are available for the repository on Windows
11 and Ubuntu 22.04. That permits the desktop, host broker, native dependencies,
and packaging tools to build for the architecture they will run on without
introducing a cross-compiler or a second Linux compatibility baseline.

## Decision

Every new production desktop release ships these package sets together:

- one universal macOS build under `macos/universal`;
- one Windows NSIS installer for each of `x86_64` and `aarch64` under the
  matching `windows/<architecture>` directory;
- one Linux AppImage and one Debian package for each of `x86_64` and `aarch64`
  under the matching `linux/<architecture>` directory.

Windows ARM64 packages build on `windows-11-arm`; Linux ARM64 packages build on
`ubuntu-22.04-arm`. The existing x86_64 runners remain unchanged. `aarch64` is
the release-contract spelling because it matches Rust target triples and
Tauri updater architecture keys.

The all-or-nothing release rule now covers both architectures. A release is not
published unless every declared package and updater signature exists, and both
the immutable manifest and `latest.json` contain the complete architecture set.
GitHub receives stable, version-free download aliases for each architecture in
addition to the versioned recovery assets.

Production release builds on macOS, Windows, and Linux enable Tidebreak's
existing background update checks, authenticated download staging, broker
quiescence, and explicit restart-to-install flow. Debug builds remain disabled.
Linux relies on Tauri's installed-bundle detection to select the matching
`linux-<architecture>-appimage` or `linux-<architecture>-deb` feed entry, so a
package is never replaced with bytes from another format.

This decision does not add Windows Authenticode signing, Linux distribution
signing, silent installation, or updater support for development builds.

## Alternatives Considered

### Keep ARM64 and cross-platform updater installation deferred

Rejected because native hosted runners now cover both package boundaries, and
the signed format-specific updater contract is already published. Continuing
to gate the runtime would preserve two intentionally temporary product gaps.

### Cross-compile ARM64 packages on the x86_64 runners

Rejected. Native desktop dependencies and packaging tools are the difficult
part of these builds, and native runners exercise the same architecture that
will launch the resulting app. Cross-compilation would add sysroot and linker
maintenance while providing weaker evidence.

### Publish ARM64 packages as optional outputs

Rejected. Partial releases make stable download links, immutable manifests,
and retries describe different products for the same version. A declared
architecture must block publication when it fails.

### Enable updater installation but keep automatic background checks disabled

Rejected because Tidebreak's update manager already downloads in the
background without replacing the running app. Installation still requires the
user's explicit restart action, so removing periodic checks would only make the
same authenticated path harder to discover.

## Consequences

Production release time and runner usage increase. The ARM64 runner images are
an additional availability dependency, and either architecture can block the
whole release. Windows packages remain unsigned in the operating-system trust
model and can still trigger SmartScreen warnings.

Windows and Linux users now enter the same release feed and broker-quiescence
contract as macOS. A package-manager or installer failure must leave the
staged update retryable and resume the old broker where possible; the updater
must never silently substitute another architecture or Linux package format.

Revisit this decision if native ARM64 runners cease to provide a stable release
baseline, if Windows Authenticode or Linux repository signing is adopted, or if
a future package format cannot participate in Tauri's authenticated
restart-to-install flow.

## Validation

- Manifest tests require Windows and Linux updater keys and artifacts for both
  `x86_64` and `aarch64`.
- Workflow-policy tests pin the native ARM64 runner labels, target triples,
  artifact transfer, stable aliases, recovery assets, and all-platform gate.
- Each native package job verifies the target-named host-broker sidecar and
  produces exactly one installer per declared format before signing.
- Release-profile updater tests keep update state enabled on the three desktop
  operating systems while debug builds remain disabled.
- Before publicizing an ARM64 release as validated, clean-machine smoke tests
  cover install, launch, sidecar startup, update staging, explicit restart,
  persistence, deep links, and uninstall for Windows ARM64 and Ubuntu ARM64.
