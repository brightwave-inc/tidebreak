# The Tidebreak self-host server image.
#
# Packages the `tidebreak` binary (crates/tidebreak-cli) built with
# tidebreak-server's `postgres` feature, which is what `Profile::SelfHost`
# needs to open the store named by `TIDEBREAK_DATABASE_URL`. The desktop crate
# is never built: `-p tidebreak-cli` selects only that binary's dependency
# closure.
#
# Build context is the WORKSPACE ROOT, so the whole Cargo workspace and the
# committed Cargo.lock are visible to a `--locked` build:
#
#   context="$(mktemp -d)"
#   scripts/stage-self-host-build-context.sh "$context"
#   docker build -f "$context/deploy/self-host/Dockerfile" -t tidebreak-self-host "$context"
#   rm -rf "$context"
#
# The deployment posture this image assumes is decision record 6: the server
# and its PostgreSQL database run inside the operator's own network, and TLS
# termination plus network exposure belong to the operator's fronting
# infrastructure. Nothing here terminates TLS. See docs/self-hosting.md.

# ---- build stage -----------------------------------------------------------
# The workspace toolchain channel (stable) via the bookworm image; the
# workspace's rust-toolchain.toml declares `stable`. Digest-pinned so a build
# does not trust the mutable tag: this is rust:1-bookworm as of 2026-08-10.
# Update the digest deliberately alongside toolchain bumps
# (`docker buildx imagetools inspect rust:1-bookworm` prints the current one).
FROM rust:1-bookworm@sha256:14bc9c5966e7b3a385794b3d5389a8765668342025fbcc7b2e3d2866ac4bd8c3 AS build
WORKDIR /src

# Copy the whole workspace. A dependency-only pre-build cache layer is a later
# optimization; correctness first — and a partial copy would have to track
# every path the workspace manifests reference.
COPY . .

# The release this image carries, reported by `tidebreak --version`. The
# workspace manifest stays at 0.0.0, so without this the binary cannot state
# which release it is. The default is the honest answer for a local build; the
# publish workflow passes the release tag. Docker exposes an ARG to the RUN
# below as an environment variable, which is where `option_env!` reads it.
ARG TIDEBREAK_VERSION="0.0.0-unreleased"

# `--features tidebreak-server/postgres` reaches through the CLI into the
# server crate's feature, because the feature lives there and the CLI takes
# tidebreak-server with default features. Without it the self-host boot has no
# PostgreSQL driver compiled in.
RUN cargo build --release --locked -p tidebreak-cli --features tidebreak-server/postgres

# ---- runtime ---------------------------------------------------------------
# Digest-pinned like the build stage: this is debian:bookworm-slim as of
# 2026-08-10.
FROM debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241

# ca-certificates: the binary's TLS stack is rustls (no OpenSSL anywhere in
# the lockfile), but parts of the graph resolve platform roots, so the system
# trust store must exist for outbound model-provider calls.
# curl: the container healthcheck below, and the two downloads that follow.
# git: code mode's only VCS. Clone, worktree, checkpoint, commit, and push all
# spawn it (crates/tidebreak-server/src/code/{clone,worktree,checkpoint,gh}.rs),
# so without it a repository cannot even be registered.
#
# Debian snapshots are immutable, so the digest-pinned base plus this dated
# repository set resolve the same transitive packages on every rebuild. Bump
# the snapshot and direct package versions together as a deliberate dependency
# update; Debian Release signatures still authenticate the snapshot metadata.
RUN rm -f /etc/apt/sources.list.d/debian.sources \
    && printf '%s\n' \
       'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian/20260810T000000Z bookworm main' \
       'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian/20260810T000000Z bookworm-updates main' \
       'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian-security/20260810T000000Z bookworm-security main' \
       > /etc/apt/sources.list \
    && apt-get -o Acquire::Check-Valid-Until=false update \
    && apt-get install -y --no-install-recommends \
       ca-certificates=20250419~deb12u1 \
       curl=7.88.1-10+deb12u15 \
       git=1:2.39.5-0+deb12u3 \
    && rm -rf /var/lib/apt/lists/*

# The GitHub CLI, which code mode spawns for pull-request create, status,
# review reads, and merge (crates/tidebreak-server/src/code/gh.rs), and which
# the coding agents reach for themselves. Tidebreak observes its
# authentication and never reads or stores a token (decision record 34), so a
# container authenticates it the way `gh` supports without a terminal:
# `GH_TOKEN` in the environment. See docs/self-hosting.md.
#
# This comes from the project's own release rather than Debian, which carries
# 2.23.0. That build has no `autoMergeRequest` JSON field, and Tidebreak asks
# for it in one request with every other field, so the whole pull-request
# digest fails against it. Pinned by version and SHA-256 from the release's
# published checksums; bump both together as a deliberate update.
RUN set -eu; \
    version=2.98.0; \
    case "$(dpkg --print-architecture)" in \
      amd64) platform=linux_amd64; \
             sha256=3b8ac6b30336802fc1a858d7c084e11cdf24ac1a761ca90b68022d7d729208de ;; \
      arm64) platform=linux_arm64; \
             sha256=cf689084f3a3618f7eae4a2420d335d74626d65f5e594b9828d125d69f800d86 ;; \
      *) echo "no gh pin for $(dpkg --print-architecture)" >&2; exit 1 ;; \
    esac; \
    archive="gh_${version}_${platform}.tar.gz"; \
    curl -fsSL --proto '=https' --tlsv1.2 -o "/tmp/${archive}" \
        "https://github.com/cli/cli/releases/download/v${version}/${archive}"; \
    printf '%s  /tmp/%s\n' "${sha256}" "${archive}" | sha256sum --check --strict -; \
    tar -xzf "/tmp/${archive}" -C /tmp "gh_${version}_${platform}/bin/gh"; \
    install -m 0755 "/tmp/gh_${version}_${platform}/bin/gh" /usr/local/bin/gh; \
    rm -rf "/tmp/${archive}" "/tmp/gh_${version}_${platform}"; \
    gh --version

# The managed Node runtime code mode installs its pinned harness packages with.
#
# Only the desktop downloads Node; the server never does. So a server image
# without it answers every harness install with "the verified managed Node
# runtime is not installed in this Tidebreak data directory" and code mode
# stays gated. The contract the server verifies lives in
# crates/tidebreak-code-execution/src/managed_node.rs: one exact directory
# carrying `bin/node`, `bin/npm`, and an `installed.json` naming this version
# and the digest of the artifact the tree was unpacked from. Nothing is
# scanned and `PATH` is never consulted, so a Node that is merely installed
# somewhere does not count.
#
# The artifact is the official nodejs.org tarball, pinned by version and
# SHA-256 exactly as the desktop pins it. These three constants must match
# `MANAGED_NODE_VERSION` and the Linux entries of `CURRENT_PIN`; bump them
# together, and scripts/self-host-image-pins.test.mjs fails the build when
# they drift apart.
RUN set -eu; \
    version=20.20.2; \
    case "$(dpkg --print-architecture)" in \
      amd64) platform=linux-x64; \
             sha256=19e56f0825510207dd904f087fe52faa0a4eb6b2aab5f0ea7a33830d04888b8b ;; \
      arm64) platform=linux-arm64; \
             sha256=47ef73d543ecf6eb19435f6c03a0ac4809b3bf0dd6b26c7c571efc2a6572a74d ;; \
      *) echo "no managed Node pin for $(dpkg --print-architecture)" >&2; exit 1 ;; \
    esac; \
    archive="node-v${version}-${platform}.tar.gz"; \
    curl -fsSL --proto '=https' --tlsv1.2 \
        -o "/tmp/${archive}" "https://nodejs.org/dist/v${version}/${archive}"; \
    printf '%s  /tmp/%s\n' "${sha256}" "${archive}" | sha256sum --check --strict -; \
    mkdir -p /opt/tidebreak/node; \
    tar -xzf "/tmp/${archive}" -C /opt/tidebreak/node; \
    mv "/opt/tidebreak/node/node-v${version}-${platform}" "/opt/tidebreak/node/${version}"; \
    rm -f "/tmp/${archive}"; \
    printf '{\n  "version": "%s",\n  "artifactSha256": "%s"\n}\n' \
        "${version}" "${sha256}" > "/opt/tidebreak/node/${version}/installed.json"; \
    export PATH="/opt/tidebreak/node/${version}/bin:${PATH}"; \
    node --version; \
    npm --version

COPY --from=build /src/target/release/tidebreak /usr/local/bin/tidebreak
COPY deploy/self-host/entrypoint.sh /usr/local/bin/tidebreak-entrypoint

# The server runs unprivileged. The uid is fixed rather than assigned by
# adduser so a data volume initialized from this layer carries an ownership
# the running user actually matches.
RUN groupadd --gid 10001 tidebreak \
    && useradd --uid 10001 --gid 10001 --home-dir /home/tidebreak --create-home \
       --shell /usr/sbin/nologin tidebreak \
    && chmod 0755 /usr/local/bin/tidebreak-entrypoint \
    && mkdir -p /var/lib/tidebreak \
    && chown -R tidebreak:tidebreak /var/lib/tidebreak /home/tidebreak

# The self-host profile. `TIDEBREAK_DATABASE_URL` and `TIDEBREAK_AUTH_TOKENS_FILE`
# have no defaults and must be supplied — the server refuses to open a shared
# store without a principal-naming authenticator, and refuses to start when
# that authenticator names no administrator.
ENV TIDEBREAK_PROFILE=self_host
# Instance lock, logs, and per-turn scratch live here; back this with a volume
# only if you want the logs to outlive the container. The durable state is in
# PostgreSQL.
ENV TIDEBREAK_DATA_DIR=/var/lib/tidebreak
# All interfaces *of this container* — which is what makes it reachable at a
# known port from outside its own network namespace. Exposure beyond the
# container is the host's decision: docker-compose.yml publishes this to
# 127.0.0.1 only, and production exposure belongs to a TLS-terminating proxy in
# front of it. Plain HTTP by design; nothing here terminates TLS.
ENV TIDEBREAK_LISTEN_ADDR=0.0.0.0:8080
EXPOSE 8080

USER tidebreak:tidebreak

HEALTHCHECK --interval=30s --timeout=5s --start-period=90s --retries=5 \
    CMD curl -fsS http://127.0.0.1:8080/healthz || exit 1

# The entrypoint publishes the image's managed Node runtime into the data
# directory before starting the server; see entrypoint.sh for why that cannot
# happen at build time. Arguments still append to `serve`, and overriding
# `--entrypoint` reaches the binary directly.
ENTRYPOINT ["/usr/local/bin/tidebreak-entrypoint"]
