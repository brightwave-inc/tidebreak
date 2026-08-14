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
# curl: the container healthcheck below, and nothing else.
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
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /src/target/release/tidebreak /usr/local/bin/tidebreak

# The server runs unprivileged. The uid is fixed rather than assigned by
# adduser so a data volume initialized from this layer carries an ownership
# the running user actually matches.
RUN groupadd --gid 10001 tidebreak \
    && useradd --uid 10001 --gid 10001 --home-dir /home/tidebreak --create-home \
       --shell /usr/sbin/nologin tidebreak \
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

ENTRYPOINT ["/usr/local/bin/tidebreak", "serve"]
