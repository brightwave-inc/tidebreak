# The OpenWave sandbox agent images.
#
# Packages the `openwave-sandbox-agent` binary: the sandbox-side transport
# server plus the in-container agent loop. The container's entrypoint runs the
# agent server on the listener port the host dials (default 8080, publishable by
# the local container backend in issue #874). The same image has a second face:
# run with the `egress-proxy` argument it serves the sandbox's network boundary
# from its own dual-homed container (see `sandbox_docker` in openwave-server).
#
# Two runtime variants build from this file:
#
#   - `slim`      — the agent binary on a minimal Debian base. The egress proxy
#                   runs from this layer's contents; nothing else is needed.
#   - `documents` — `slim` plus LibreOffice (headless DOCX/PPTX/XLSX rendering
#                   for the document skills' visual QA loop, and the UNO bridge
#                   for driving Calc from Python), Node.js with pptxgenjs for
#                   JavaScript deck generation, the bundled exec helper scripts,
#                   and the document skills' pinned Python dependencies
#                   preinstalled system-wide, so a sandbox produces documents
#                   without any in-run package install. This is the default
#                   build target and the ref the local backend runs.
#
# Build context is the WORKSPACE ROOT, so the whole Cargo workspace and the
# committed Cargo.lock are visible to a `--locked` build:
#
#   docker build -f crates/openwave-sandbox-agent/Dockerfile -t openwave-sandbox-agent .
#   docker build -f crates/openwave-sandbox-agent/Dockerfile --target slim -t openwave-sandbox-agent-slim .
#
# Official images are published to GHCR by .github/workflows/publish-sandbox-image.yml
# on version tags, and the local backend pins the published documents image by
# digest (see `sandbox_docker` in openwave-server for the recorded pin and its
# provenance). A locally built tag is the development fallback and carries no
# digest verification.
#
# The scoped post-merge sandbox-resident lane builds and drives this image on a
# real container boundary.

# ---- build stage -----------------------------------------------------------
# The workspace toolchain channel (stable) via the bookworm image; the
# workspace's rust-toolchain.toml declares `stable`. Digest-pinned so the
# publish moment does not trust the mutable tag: this is rust:1-bookworm as of
# 2026-08-03. Update the digest deliberately alongside toolchain bumps
# (`docker buildx imagetools inspect rust:1-bookworm` prints the current one).
FROM rust:1-bookworm@sha256:77fac8b98f9f46062bb680b6d25d5bcaabfc400143952ebc572e924bcbedc3fa AS build
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

# ---- slim runtime ----------------------------------------------------------
# The agent binary and nothing else: the egress proxy's image, and the base the
# documents variant layers onto. Digest-pinned like the build stage: this is
# debian:bookworm-slim as of 2026-08-03.
FROM debian:bookworm-slim@sha256:7b140f374b289a7c2befc338f42ebe6441b7ea838a042bbd5acbfca6ec875818 AS slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /workspace

COPY --from=build /src/target/release/openwave-sandbox-agent /usr/local/bin/openwave-sandbox-agent

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
EXPOSE 8080

USER openwave:openwave

ENTRYPOINT ["/usr/local/bin/openwave-sandbox-agent"]

# ---- Node.js distribution --------------------------------------------------
# The Node.js runtime the documents stage copies in. The official Node image
# built on the same Debian release as the runtime base, so the binary links
# against the glibc that is already there and no third-party apt repository or
# per-architecture tarball URL enters the build. Digest-pinned like every other
# base here: this is the multi-architecture node:20-bookworm-slim index as of
# 2026-08-06 (Node 20.20.2), covering the amd64 and arm64 images the publish
# workflow builds. Update the digest deliberately (`docker buildx imagetools
# inspect node:20-bookworm-slim` prints the current one).
FROM node:20-bookworm-slim@sha256:2cf067cfed83d5ea958367df9f966191a942351a2df77d6f0193e162b5febfc0 AS nodedist

# ---- documents runtime (default) -------------------------------------------
# The document helpers need Python plus native renderers, and the document
# skills (skills/*/SKILL.md) each pin one Python library. Preinstalling those
# exact pins system-wide means a background document run needs no in-sandbox
# `pip install` — no package-manager egress, no per-run install latency. The
# container remains the isolation boundary; packages are installed at
# image-build time and no helper performs a network request at runtime.
FROM slim AS documents

USER root

# The pip pins live in documents-requirements.txt: the SKILL.md manifests'
# exact versions plus their pinned transitive closure, every entry
# hash-checked, so a substituted PyPI artifact fails the build instead of
# shipping. Regenerate the file with scripts/generate-documents-requirements.py
# when a SKILL.md bumps a version; the sandbox-image-pins test enforces the
# lockstep. Pillow rides the hash-checked closure rather than apt so its
# version is pinned with the rest.
COPY crates/openwave-sandbox-agent/documents-requirements.txt /tmp/documents-requirements.txt

# The apt set is the native side of the same bargain: the three LibreOffice
# document components the skills render and convert through, poppler for PDF
# rasterization and figure extraction, and Python for the helper scripts.
# `python3-uno` is LibreOffice's Python bridge — it binds to the *system*
# python3, which is why the helpers run under `python3` rather than a virtual
# environment, and it lets a script drive Calc (formulas, chart objects,
# recalculation) instead of only writing files another tool reopens.
# `default-jre-headless` is what LibreOffice's own conversion filters need for
# the paths that route through Java; without it those conversions fail at run
# time inside a network-free sandbox.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       default-jre-headless \
       libreoffice-calc \
       libreoffice-impress \
       libreoffice-writer \
       poppler-utils \
       python3 \
       python3-pip \
       python3-uno \
    && python3 -m pip install --break-system-packages --no-cache-dir \
       --require-hashes -r /tmp/documents-requirements.txt \
    && rm -rf /var/lib/apt/lists/* /root/.cache /tmp/documents-requirements.txt \
    && mkdir -p /opt/openwave/exec-scripts

# Node.js, for the JavaScript deck path: pptxgenjs builds a PPTX from script
# without the layout gymnastics the Python route needs for the same slide. Only
# the interpreter and the bundled npm are taken from the pinned distribution
# image — the `npm`/`npx` entry points are the symlinks that image ships,
# recreated here explicitly so what lands on PATH is visible in this file.
COPY --from=nodedist /usr/local/bin/node /usr/local/bin/node
COPY --from=nodedist /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
    && ln -s ../lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx

# The deck library, pinned exactly like the Python closure and installed
# globally at build time so a run needs no `npm install` and no registry
# egress. npm verifies each package against the registry's integrity hash on
# install, which is the equivalent of pip's `--require-hashes` here.
RUN npm install --global --no-audit --no-fund --loglevel=error pptxgenjs@4.0.1 \
    && npm cache clean --force \
    && rm -rf /root/.npm

# Global installs are not on Node's default resolution path for a script run
# from the workspace, so point NODE_PATH at them: `require("pptxgenjs")`
# resolves from any directory, with no per-workspace node_modules.
ENV NODE_PATH=/usr/local/lib/node_modules

COPY scripts/exec-documents/ /opt/openwave/exec-scripts/

RUN python3 /opt/openwave/exec-scripts/render_pdf.py --help >/dev/null \
    && python3 /opt/openwave/exec-scripts/analyze_xlsx.py --help >/dev/null \
    && python3 /opt/openwave/exec-scripts/calc_uno.py --help >/dev/null \
    && python3 /opt/openwave/exec-scripts/xlsx_recalc.py --help >/dev/null \
    && python3 -c "import uno" \
    && node -e "require('pptxgenjs')"

# The helper library path shown to the model-facing exec tool.
ENV OPENWAVE_EXEC_SCRIPTS=/opt/openwave/exec-scripts

USER openwave:openwave
