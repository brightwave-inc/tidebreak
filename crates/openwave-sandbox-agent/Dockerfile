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
# Nothing verifies this image by digest today. The host runs whatever ref it is
# configured with (`DockerConfig::image`), which accepts a digest-pinned ref but
# does not require or check one. Treat the image's integrity as unverified on the
# local backend.
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

# The agent — and therefore every command the model runs through `exec` — runs as
# an unprivileged user, so a container escape starts from an account that owns
# nothing but the workspace. The uid is fixed rather than assigned by adduser so
# the host can force the same identity with `--user` (see `sandbox_docker`'s
# `SANDBOX_USER`) even against an image that forgot this directive, and so a
# workspace volume initialized from this layer carries an ownership the running
# user actually matches.
RUN groupadd --gid 10001 openwave \
    && useradd --uid 10001 --gid 10001 --home-dir /home/openwave --create-home \
       --shell /usr/sbin/nologin openwave \
    && chown -R openwave:openwave /workspace /home/openwave

# The listener address the supervisor binds; the host dials this port.
ENV OPENWAVE_SANDBOX_LISTEN=0.0.0.0:8080
# The in-container workspace root the exec and filesystem tools are scoped to.
ENV OPENWAVE_SANDBOX_WORKSPACE=/workspace
# The helper library path shown to the model-facing exec tool.
ENV OPENWAVE_EXEC_SCRIPTS=/opt/openwave/exec-scripts
EXPOSE 8080

USER openwave:openwave

ENTRYPOINT ["/usr/local/bin/openwave-sandbox-agent"]
