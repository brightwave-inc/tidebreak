# 12. Adopt Tidebreak as the Product and Technical Identity

- Status: Accepted
- Date: 2026-08-12
- Owners: product and platform
- Related: repository, release, desktop, CLI, storage, and deployment naming
- Supersedes: none

## Context

The product previously called OpenWave has been renamed Tidebreak. The
`tidebreak.sh`, `tidebreak.dev`, and `tidebreak.io` domains are owned for the
new identity. There are no existing users or installations whose commands,
configuration, application data, persisted state, or operating-system
registration need compatibility handling.

The GitHub repository remains `brightwave-inc/openwave` temporarily and will be
renamed separately. Its current URL is an operational location, not a product
identifier. New GitHub Container Registry packages use Tidebreak names now.

## Decision

Tidebreak is the sole canonical product and technical name. Product-owned
names use `Tidebreak`, `tidebreak`, or `TIDEBREAK` as appropriate, including:

- Rust crates, libraries, binaries, modules, and dependency names;
- desktop product, bundle, deep-link, executable, and sidecar identifiers;
- UI and documentation packages;
- commands, configuration keys, environment variables, service names, and
  application-data paths;
- release artifacts, deployment resources, images, generated files, tests,
  examples, documentation, and source terminology.

No OpenWave aliases, migrations, shims, or deprecation period will be added.
An old product-owned name left after the rename is a defect.

Until the repository is renamed, references that must resolve to the current
GitHub repository retain `brightwave-inc/openwave`. Sandbox images publish to
new `tidebreak-sandbox-agent` GitHub Container Registry packages immediately.
The later repository move must update the remaining repository coordinates and
is deliberately outside this change.

## Alternatives Considered

### Public-facing rebrand only

Keeping OpenWave as the internal crate, command, storage, and configuration
namespace would reduce the immediate edit surface. It was rejected because it
would leave two permanent identities, make documentation and diagnostics
inconsistent, and turn every future technical surface into a naming choice.

### Compatibility transition

Accepting both names for commands, environment variables, data paths, and app
registration would protect installed users during a staged migration. It was
rejected because there are no existing users or installations to migrate, so
the aliases would create complexity without preserving real data or workflows.

### Rename the GitHub repository first

Moving the repository before the in-tree rename would make its coordinates
match immediately. It was rejected for this phase because the repository move
can happen independently after code, automation, artifacts, and documentation
are ready, while GitHub and GHCR coordinates must continue resolving in the
meantime.

### Do nothing

Retaining OpenWave was rejected because Tidebreak is the chosen product name
and the relevant domains have been acquired.

## Consequences

This is an intentionally breaking rename of every product-owned technical
surface. Existing scripts or local development state using OpenWave names are
not supported, but there are no users for whom that is a compatibility cost.
Contributors must use Tidebreak terminology even while the checkout or GitHub
URL still contains `openwave`.

The repository rename remains follow-up work. At that point, repository URLs,
security links, badges, and workflow subjects must move, with release
publication checked after the change.

Revisit this decision only if an undiscovered external installation or
published contract predating the rename is found and losing it would cause
material user data loss or prevent a required upgrade.

## Validation

- No tracked filename contains `openwave` outside Git administrative paths.
- Case-insensitive source searches find the old name only in accepted historical
  decision records and explicitly documented current GitHub coordinates.
- Cargo metadata resolves only Tidebreak workspace packages and binaries.
- Locked Rust and UI builds and their focused tests pass without dependency
  version drift.
- Desktop metadata, release artifact names, environment variables, data paths,
  deep links, service identifiers, and documentation all identify Tidebreak.
