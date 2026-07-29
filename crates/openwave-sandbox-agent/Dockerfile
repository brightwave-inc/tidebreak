# The OpenWave sandbox agent image.
#
# Packages the `openwave-sandbox-agent` binary: the sandbox-side transport
# server plus the in-container agent loop. The container's entrypoint runs the
# agent server on the listener port the host dials (default 8080, publishable by
# the local container backend in issue #874).
#
# Build context is the WORKSPACE ROOT, so the whole Cargo workspace and the
# committed Cargo.lock are visible to a `--locked` build:
#
#   docker build -f crates/openwave-sandbox-agent/Dockerfile -t openwave-sandbox-agent .
#
# The host verifies this image by digest before provisioning (a constraint from
# issue #873); pinning-by-digest and host verification live on the host side in
# #874, not in the image itself.
#
# Image build/verification for this slice is intentionally MANUAL, not a CI lane:
# building it compiles the crate's slice of the workspace inside Docker, which is
# far heavier than the cheap gates this foundational PR runs, and the container
# runtime is a detected capability rather than a hard dependency. The command
# above is the manual check.

# ---- build stage -----------------------------------------------------------
# Pinned to the workspace toolchain channel (stable) via the bookworm image; the
# workspace's rust-toolchain.toml declares `stable`.
FROM rust:1-bookworm AS build
WORKDIR /src

# Copy the whole workspace. A dependency-only pre-build cache layer is a later
# optimization; correctness first.
COPY . .

# Build only the agent crate. Its dependency on openwave-core carries
# `default-features = false`, so sea-orm, the OS keychain, and the filesystem
# tools are never compiled into the image — no model or third-party credential
# ships in it.
RUN cargo build --release --locked -p openwave-sandbox-agent

# ---- runtime stage ---------------------------------------------------------
# distroless/cc provides glibc and libgcc for the dynamically-linked binary and
# nothing else — no shell, no package manager — a minimal attack surface for a
# container that runs untrusted model output. The binary uses rustls (no OpenSSL)
# and opens no keychain, so cc is sufficient.
FROM gcr.io/distroless/cc-debian12 AS runtime

COPY --from=build /src/target/release/openwave-sandbox-agent /usr/local/bin/openwave-sandbox-agent

# The listener address the supervisor binds; the host dials this port.
ENV OPENWAVE_SANDBOX_LISTEN=0.0.0.0:8080
EXPOSE 8080

# distroless has no shell, so use the exec form.
ENTRYPOINT ["/usr/local/bin/openwave-sandbox-agent"]
