# Security Policy

## Reporting a vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

Instead, report them privately through GitHub's
[private vulnerability reporting](https://github.com/brightwave-inc/tidebreak/security/advisories/new)
(**Security** tab → **Report a vulnerability**).

Please include enough detail to reproduce: affected component/version, steps,
and impact. We'll acknowledge your report, keep you updated on progress, and
coordinate disclosure once a fix is available.

## Supported versions

The latest release receives security fixes. An earlier release stops receiving
them as soon as a newer one ships, so to get a fix, update to the latest
release. There are no separate release branches.

Fixes land on `main` and ship in a new release:

- The desktop app installs the new release through its automatic updater. The
  CLI ships inside the desktop app and updates with it.
- A self-hosted headless server gets the fix when its operator upgrades, as
  described in [Upgrading](docs/self-hosting.md#upgrading).
- The hosted machine adopts each new release on its own.
- The mobile client receives the fix as an over-the-air update or a new build.

## Scope

This policy covers the desktop app, the headless server, the CLI, the hosted
machine, and the mobile client.
