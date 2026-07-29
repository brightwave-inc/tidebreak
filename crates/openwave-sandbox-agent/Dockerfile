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
# `default-features = false`, so sea-orm, the OS keychain, and openwave-core's
# own feature-gated filesystem tools are never compiled into the image — no model
# or third-party credential ships in it. (The sandbox-resident `exec`/filesystem
# tools live in this crate and are the point of the image.)
RUN cargo build --release --locked -p openwave-sandbox-agent

# The sandbox-resident `exec` tool runs model-authored commands through /bin/sh
# inside the container — in-container execution IS the containment. distroless/cc
# ships no shell, so stage a static busybox (a shell plus the common coreutils
# applets) and the workspace root the tools are scoped to, then copy just those
# into the runtime. busybox is a deliberate, bounded surface for running the
# model's commands, not a general package manager.
#
# The applet symlinks must resolve in the RUNTIME image, where this staging tree
# lands at `/`. `busybox --install -s` would bake in the *build-time* absolute
# path (`/rootfs/bin/busybox`), which does not exist after the copy — every
# symlink would dangle and `/bin/sh -c` would fail for every exec. So create each
# applet as a RELATIVE symlink to `busybox`: at runtime `/bin/sh -> busybox`
# resolves to `/bin/busybox`, which is there. Correct by construction regardless
# of where the staging tree was assembled.
RUN apt-get update \
    && apt-get install -y --no-install-recommends busybox-static \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /rootfs/bin /rootfs/workspace \
    && cp /bin/busybox /rootfs/bin/busybox \
    && for applet in $(/rootfs/bin/busybox --list); do \
         [ "$applet" = busybox ] || ln -s busybox /rootfs/bin/"$applet"; \
       done \
    && test "$(readlink /rootfs/bin/sh)" = busybox

# ---- runtime stage ---------------------------------------------------------
# distroless/cc provides glibc and libgcc for the dynamically-linked binary; the
# binary uses rustls (no OpenSSL) and opens no keychain, so cc is sufficient. It
# ships no package manager — the only added surface is the staged busybox the
# exec tool shells out to, in a container that runs untrusted model output.
FROM gcr.io/distroless/cc-debian12 AS runtime

COPY --from=build /src/target/release/openwave-sandbox-agent /usr/local/bin/openwave-sandbox-agent
# busybox (/bin/sh + coreutils applets) and the /workspace tool root.
COPY --from=build /rootfs /

# The listener address the supervisor binds; the host dials this port.
ENV OPENWAVE_SANDBOX_LISTEN=0.0.0.0:8080
# The in-container workspace root the exec and filesystem tools are scoped to.
ENV OPENWAVE_SANDBOX_WORKSPACE=/workspace
EXPOSE 8080

# distroless has no shell for a RUN, so use the exec form for the entrypoint.
ENTRYPOINT ["/usr/local/bin/openwave-sandbox-agent"]
