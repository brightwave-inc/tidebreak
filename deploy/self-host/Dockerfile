# The OpenWave self-host server image.
#
# Packages the `openwave` binary (crates/openwave-cli) built with
# openwave-server's `postgres` feature, which is what `Profile::SelfHost`
# needs to open the store named by `OPENWAVE_DATABASE_URL`. The desktop crate
# is never built: `-p openwave-cli` selects only that binary's dependency
# closure.
#
# Build context is the WORKSPACE ROOT, so the whole Cargo workspace and the
# committed Cargo.lock are visible to a `--locked` build:
#
#   docker build -f deploy/self-host/Dockerfile -t openwave-self-host .
#
# The deployment posture this image assumes is decision record 4: the server
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

# `--features openwave-server/postgres` reaches through the CLI into the
# server crate's feature, because the feature lives there and the CLI takes
# openwave-server with default features. Without it the self-host boot has no
# PostgreSQL driver compiled in.
RUN cargo build --release --locked -p openwave-cli --features openwave-server/postgres

# ---- runtime ---------------------------------------------------------------
# Digest-pinned like the build stage: this is debian:bookworm-slim as of
# 2026-08-10.
FROM debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241

# ca-certificates: the binary's TLS stack is rustls (no OpenSSL anywhere in
# the lockfile), but parts of the graph resolve platform roots, so the system
# trust store must exist for outbound model-provider calls.
# curl: the container healthcheck below.
# socat: the loopback bridge the entrypoint needs — see entrypoint.sh for why
# it is here and what removes it.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl socat \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /src/target/release/openwave /usr/local/bin/openwave
COPY deploy/self-host/entrypoint.sh /usr/local/bin/openwave-entrypoint

# The server runs unprivileged. The uid is fixed rather than assigned by
# adduser so a data volume initialized from this layer carries an ownership
# the running user actually matches.
RUN groupadd --gid 10001 openwave \
    && useradd --uid 10001 --gid 10001 --home-dir /home/openwave --create-home \
       --shell /usr/sbin/nologin openwave \
    && mkdir -p /var/lib/openwave \
    && chown -R openwave:openwave /var/lib/openwave /home/openwave \
    && chmod 0755 /usr/local/bin/openwave-entrypoint

# The self-host profile. `OPENWAVE_DATABASE_URL` and `OPENWAVE_AUTH_TOKENS_FILE`
# have no defaults and must be supplied — the server refuses to open a shared
# store without a principal-naming authenticator.
ENV OPENWAVE_PROFILE=self_host
# Instance lock, logs, and per-turn scratch live here; back this with a volume
# only if you want the logs to outlive the container. The durable state is in
# PostgreSQL.
ENV OPENWAVE_DATA_DIR=/var/lib/openwave
# The port the entrypoint's bridge publishes. Plain HTTP by design.
ENV OPENWAVE_LISTEN_PORT=8080
EXPOSE 8080

USER openwave:openwave

HEALTHCHECK --interval=30s --timeout=5s --start-period=90s --retries=5 \
    CMD curl -fsS "http://127.0.0.1:${OPENWAVE_LISTEN_PORT}/healthz" || exit 1

ENTRYPOINT ["/usr/local/bin/openwave-entrypoint"]
