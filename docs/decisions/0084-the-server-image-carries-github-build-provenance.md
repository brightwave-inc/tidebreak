# 84. The server image carries GitHub build provenance

- Status: Accepted
- Date: 2026-09-02
- Owners: server
- Related: [`0047-gateway-linked-hosting.md`](0047-gateway-linked-hosting.md),
  [`0082-the-hosted-machine-serves-the-renderer.md`](0082-the-hosted-machine-serves-the-renderer.md),
  [`../self-hosting.md`](../self-hosting.md),
  [`.github/workflows/publish-server-image.yml`](../../.github/workflows/publish-server-image.yml)
- Supersedes: none

## Context

`publish-server-image.yml` builds `ghcr.io/brightwave-inc/tidebreak-server`
on every release, on a weekly schedule that flushes base-image patches into
the same version tag, and on manual dispatch. A consumer that pins the image
pins a digest, and nothing on the registry says where that digest came from:
a digest pushed by a package-write token looks exactly like one the workflow
built.

Model Gateway hosts Tidebreak as a managed add-on and admits images against
a trust policy. Today that policy enumerates digests, so every Tidebreak
release needs a deployment change to admit the new digest, and a weekly
rebuild that moves a version tag onto patched bases needs one too. The
gateway wants to replace the enumerated list with a rule: admit any digest
that this repository's publish workflow provably built. That rule needs a
signed statement it can verify without trusting the registry or the network
path to it.

GitHub artifact attestations provide one. `actions/attest-build-provenance`
signs an in-toto statement carrying a SLSA v1 provenance predicate with a
Sigstore certificate issued to the workflow's OIDC identity, records it in
the public transparency log, and stores the bundle under the repository. A
verifier checks the bundle offline against the public Sigstore trust root
and reads the signing identity from the certificate, not from the
statement.

## Decision

1. **Every published server image is attested.** The per-architecture
   manifests that the build job pushes and the multi-architecture index the
   manifest job assembles each get a build-provenance attestation. All three
   publish lanes attest, the weekly rebuild included, because a rebuilt
   digest is exactly the digest a consumer next adopts.
2. **Attestations are stored with the repository and pushed to the
   registry.** The bundle lands in GitHub's attestation store for
   `brightwave-inc/tidebreak` and is also attached to the image as an OCI
   referrer, so `gh attestation verify` and `cosign` both find it.
3. **The workflow path is a public contract.** A verifier identifies the
   image by the signing certificate's identity:
   `https://github.com/brightwave-inc/tidebreak/.github/workflows/publish-server-image.yml`
   followed by the ref the run used. Renaming or moving that file changes
   the identity of every image published afterwards and must be treated as
   a breaking change to consumers that verify provenance.
4. **The action is pinned by commit and moved by Dependabot**, like every
   other action in the file.

Deliberately excluded: signing with a repository-held key, attesting the
desktop bundles, and any change to how the image is built.

## Alternatives Considered

- **Do nothing; consumers keep enumerating digests.** Every release and
  every weekly rebuild then costs a deployment change somewhere else, and
  the list says nothing about where a digest came from.
- **Cosign keyless signing from the workflow.** Equivalent trust, but a
  second signing tool with its own pinning and its own storage convention,
  while the attestation action is first-party and its bundles are readable
  through the GitHub API without registry credentials.
- **Attest only the index digest.** Enough for the gateway, which resolves
  a tag to the index. The per-architecture attestations cost one step each
  and let an operator who pins a single-platform manifest verify it too.

## Consequences

- The build and manifest jobs gain `id-token: write` and
  `attestations: write`. Neither grants anything beyond signing with the
  workflow's own identity and writing bundles under this repository.
- A package-write token can still push to GHCR, but what it pushes carries
  no attestation and a provenance-verifying consumer refuses it. That is
  the point.
- The workflow file name is now load-bearing outside this repository.
- Revisit if GitHub changes the attestation storage or Sigstore trust
  domain for public repositories, or if a consumer needs a predicate the
  build-provenance action does not emit.

## Validation

- `gh attestation verify oci://ghcr.io/brightwave-inc/tidebreak-server:<version> -R brightwave-inc/tidebreak`
  succeeds for the first release published after this record merges, and
  the printed signer identity names `publish-server-image.yml`.
- The same command against a digest pushed outside the workflow reports no
  attestation, which is the refusal a consumer must produce.
- A wrong implementation that attested only the per-architecture manifests
  would still pass a per-platform verify; the check above runs against the
  version tag, which resolves to the index.
