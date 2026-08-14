# 22. Release Tags Publish Static Documentation

- Status: Proposed
- Date: 2026-08-14
- Owners: release engineering and documentation
- Related: `docs/releases.md`, `docs-site/README.md`
- Supersedes: none

## Context

Tidebreak's documentation describes a released desktop product and is built
from source in this repository. Building it from a mutable branch can publish
behavior that users cannot install yet. Building it inside the marketing-site
deployment instead couples two repositories, requires cross-repository source
access during an unrelated build, and makes a marketing rollback also a docs
build operation.

The documentation application already exports static files and supports the
`/docs` base path. The marketing site can expose a fixed separately deployed
origin through an edge rewrite without running an application proxy.

## Decision

Publishing a non-prerelease GitHub Release is the documentation publication
boundary. The trusted release workflow validates the tag, resolves its exact
commit on `main`, checks out that commit, and builds `docs-site/` with
`BASE_PATH=/docs`.

The static export is packaged beneath `/docs` in Vercel's prebuilt output,
rather than relying on framework-generated routes that store the exported
files at the deployment root. The deployment therefore serves `/docs` as a
real static path at its own origin as well as through the marketing rewrite.

The build is deployed first as an unaliased production-target deployment in
the dedicated `tidebreak-docs` project. The workflow smoke-tests the staged
deployment through Vercel's authenticated deployment client, then promotes
that immutable deployment to the project's production aliases. Deployment
metadata records the release tag and commit SHA, and the workflow retains a
digest manifest for the generated static files.

The marketing site remains static. It rewrites `/docs` and `/docs/*` to the
fixed docs production alias. It does not clone this repository, rebuild the
documentation, or proxy requests through an Astro server route.

Only the publication job receives the Vercel credential, through the
`docs-production` GitHub environment. Documentation build dependencies and
the Vercel CLI are exact-version lockfile inputs. No repository setting is
changed by this decision or its implementation; the environment secret must
be configured separately before the first release publication.

## Alternatives Considered

- Deploy documentation on every merge to `main`. Rejected because public docs
  could describe unreleased behavior and would not identify the product
  version they accompany.
- Clone and build Tidebreak from the marketing-site deployment. Rejected
  because it is a fail-open cross-repository build dependency and does not
  prove which release the documentation represents.
- Run a marketing-site application proxy in front of a docs deployment.
  Rejected because the edge platform can perform the fixed rewrite without a
  server runtime, response-header relay, or additional availability boundary.
- Commit generated docs into the marketing repository. Rejected because large
  generated diffs obscure review, duplicate source of truth, and require a
  cross-repository writer token.

## Consequences

Documentation updates intentionally wait for a published release. A docs-only
correction therefore requires a patch release unless a future policy creates a
separately versioned documentation channel.

The release workflow gains a dependency on the Vercel API and one scoped
deployment credential. A failed docs deployment fails the release workflow but
does not replace the current production docs deployment, because promotion
happens only after staged smoke tests pass. The previous deployment remains
available for rollback through Vercel.

Revisit this decision if documentation becomes independently versioned, if
multiple maintained product versions require parallel docs, or if the docs
host changes to a content-addressed object store with equivalent staged
promotion and rollback behavior.

## Validation

Workflow policy tests must prove that the job checks out the validated release
SHA, builds with `/docs`, scopes credentials to the publication job, deploys
without immediately assigning production aliases, smoke-tests the staged
deployment, and only then promotes it. A release rehearsal must verify the
root page, a nested page, static Next assets, search index, sitemap, and
canonical metadata through both the staged URL and the public `/docs` path.
