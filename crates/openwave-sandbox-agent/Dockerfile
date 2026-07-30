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
# The scoped post-merge sandbox-resident lane builds and drives this image on a
# real container boundary.

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

# ---- runtime stage ---------------------------------------------------------
# The document helpers need Python plus native renderers. The container remains
# the isolation boundary; packages are installed at image-build time and no
# helper performs a network request at runtime.
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       ca-certificates \
       libreoffice-impress \
       libreoffice-writer \
       poppler-utils \
       python3 \
       python3-openpyxl \
       python3-pil \
       python3-pip \
    && python3 -m pip install --break-system-packages --no-cache-dir pypdfium2==4.30.0 \
    && rm -rf /var/lib/apt/lists/* /root/.cache \
    && mkdir -p /workspace /opt/openwave/exec-scripts

COPY --from=build /src/target/release/openwave-sandbox-agent /usr/local/bin/openwave-sandbox-agent
COPY scripts/exec-documents/ /opt/openwave/exec-scripts/

RUN python3 /opt/openwave/exec-scripts/render_pdf.py --help >/dev/null \
    && python3 /opt/openwave/exec-scripts/analyze_xlsx.py --help >/dev/null

# The listener address the supervisor binds; the host dials this port.
ENV OPENWAVE_SANDBOX_LISTEN=0.0.0.0:8080
# The in-container workspace root the exec and filesystem tools are scoped to.
ENV OPENWAVE_SANDBOX_WORKSPACE=/workspace
# The helper library path shown to the model-facing exec tool.
ENV OPENWAVE_EXEC_SCRIPTS=/opt/openwave/exec-scripts
EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/openwave-sandbox-agent"]
