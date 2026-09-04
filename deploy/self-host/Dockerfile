# The Tidebreak self-host server image.
#
# Packages the `tidebreak` binary (crates/tidebreak-cli) built with
# tidebreak-server's `postgres` feature, which is what `Profile::SelfHost`
# needs to open the store named by `TIDEBREAK_DATABASE_URL`. The desktop crate
# is never built: `-p tidebreak-cli` selects only that binary's dependency
# closure. The desktop's renderer is: the image carries the built UI bundle
# and the server serves it, so a browser can land on the machine
# (decision record 82).
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

# ---- renderer stage --------------------------------------------------------
# The same `pnpm build` the desktop packages as `frontendDist`, so a browser
# tab and the desktop app run one renderer against one API. Node 22 and pnpm
# 10 match the desktop UI lane in .github/workflows/ci.yml. Digest-pinned
# like the stages below: this is node:22-bookworm as of 2026-09-02.
FROM node:22-bookworm@sha256:8a34c4ab3ea2c5cd194f07e317b2a8f09461d3c8b05c4e34c8ccd56d56024c4d AS renderer
WORKDIR /src/crates/tidebreak-desktop/ui

# Manifests first, so the dependency layer survives a source-only change.
COPY crates/tidebreak-desktop/ui/package.json \
     crates/tidebreak-desktop/ui/pnpm-lock.yaml \
     crates/tidebreak-desktop/ui/pnpm-workspace.yaml ./
# `--ignore-scripts`: the lockfile's only native build (canvas) serves the
# test environment, and a production bundle never loads it. Skipping every
# install script also keeps the image build off the dependency tree's
# lifecycle hooks, which is where a supply-chain compromise runs first.
RUN npm install -g pnpm@10.34.5 \
    && pnpm install --frozen-lockfile --ignore-scripts

COPY crates/tidebreak-desktop/ui/ ./
RUN pnpm build

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

# `release` is the image operators run. `dev` builds the same binary with debug
# assertions on, which is the only build that carries the scripted harness
# (`crates/tidebreak-server/src/scripted_harness.rs`); the end-to-end lane in
# the Model Gateway repository drives a real machine through it. A `dev` image
# is a test fixture: never deploy one.
ARG CARGO_PROFILE=release
# Debug info is dead weight in a container and triples the dev binary.
ENV CARGO_PROFILE_DEV_DEBUG=0

# `--features tidebreak-server/postgres` reaches through the CLI into the
# server crate's feature, because the feature lives there and the CLI takes
# tidebreak-server with default features. Without it the self-host boot has no
# PostgreSQL driver compiled in. The binary lands in `/out` so the runtime
# stage copies one path whichever profile built it (cargo writes `dev` to
# `target/debug`).
RUN set -eu; \
    cargo build --profile "${CARGO_PROFILE}" --locked -p tidebreak-cli \
        --features tidebreak-server/postgres; \
    case "${CARGO_PROFILE}" in dev) target_dir=debug ;; *) target_dir="${CARGO_PROFILE}" ;; esac; \
    install -D "target/${target_dir}/tidebreak" /out/tidebreak

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

COPY --from=build /out/tidebreak /usr/local/bin/tidebreak
COPY --from=renderer /src/crates/tidebreak-desktop/ui/dist /opt/tidebreak/ui
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

# Optional toolchain bundles. Empty (the default) installs nothing, so this
# RUN is a no-op and the image stays the size of git, gh, and managed Node.
# Pass --build-arg TOOLCHAINS=rust,python,go,jvm to add bundles; each name
# is one delimited block below. An unknown name fails the build. Pins live
# in TOOLCHAINS.md next to this file. The label records what this digest
# was built with (decision record 84).
ARG TOOLCHAINS=""
LABEL io.tidebreak.toolchains="${TOOLCHAINS}"
RUN set -eu; \
    arch="$(dpkg --print-architecture)"; \
    for bundle in $(printf '%s' "${TOOLCHAINS}" | tr ',' ' '); do \
      bundle="$(printf '%s' "${bundle}" | tr -d '[:space:]')"; \
      [ -n "${bundle}" ] || continue; \
      case "${bundle}" in \
        rust) \
          # rust: rustup with pinned stable, cargo, clippy, rustfmt, and a
          # C toolchain so `cargo test` can link. Homes sit under /usr/local
          # so the non-root user can execute them, matching managed Node.
          rustup_version=1.27.1; \
          rust_toolchain=1.97.1; \
          case "${arch}" in \
            amd64) rustup_triple=x86_64-unknown-linux-gnu; \
                   rustup_sha256=6aeece6993e902708983b209d04c0d1dbb14ebb405ddb87def578d41f920f56d ;; \
            arm64) rustup_triple=aarch64-unknown-linux-gnu; \
                   rustup_sha256=1cffbf51e63e634c746f741de50649bbbcbd9dbe1de363c9ecef64e278dba2b2 ;; \
            *) echo "no rustup pin for ${arch}" >&2; exit 1 ;; \
          esac; \
          apt-get -o Acquire::Check-Valid-Until=false update; \
          apt-get install -y --no-install-recommends \
            build-essential=12.9; \
          rm -rf /var/lib/apt/lists/*; \
          curl -fsSL --proto '=https' --tlsv1.2 \
            -o /tmp/rustup-init \
            "https://static.rust-lang.org/rustup/archive/${rustup_version}/${rustup_triple}/rustup-init"; \
          printf '%s  /tmp/rustup-init\n' "${rustup_sha256}" | sha256sum --check --strict -; \
          chmod 0755 /tmp/rustup-init; \
          export CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup; \
          /tmp/rustup-init -y --no-modify-path \
            --default-toolchain "${rust_toolchain}" \
            --profile minimal \
            --component rustfmt,clippy; \
          rm -f /tmp/rustup-init; \
          ln -sf /usr/local/cargo/bin/cargo /usr/local/bin/cargo; \
          ln -sf /usr/local/cargo/bin/rustc /usr/local/bin/rustc; \
          ln -sf /usr/local/cargo/bin/rustup /usr/local/bin/rustup; \
          ln -sf /usr/local/cargo/bin/rustfmt /usr/local/bin/rustfmt; \
          ln -sf /usr/local/cargo/bin/cargo-clippy /usr/local/bin/cargo-clippy; \
          ln -sf /usr/local/cargo/bin/clippy-driver /usr/local/bin/clippy-driver; \
          chmod -R a+rX /usr/local/cargo /usr/local/rustup; \
          cargo --version; \
          rustc --version; \
          ;; \
        python) \
          # python: Debian python3, pip, and venv from the same snapshot as git.
          apt-get -o Acquire::Check-Valid-Until=false update; \
          apt-get install -y --no-install-recommends \
            python3=3.11.2-1+b1 \
            python3-pip=23.0.1+dfsg-1 \
            python3-venv=3.11.2-1+b1; \
          rm -rf /var/lib/apt/lists/*; \
          python3 --version; \
          pip3 --version; \
          ;; \
        go) \
          # go: pinned release tarball, SHA-256 checked before unpack.
          go_version=1.25.1; \
          case "${arch}" in \
            amd64) go_platform=linux-amd64; \
                   go_sha256=7716a0d940a0f6ae8e1f3b3f4f36299dc53e31b16840dbd171254312c41ca12e ;; \
            arm64) go_platform=linux-arm64; \
                   go_sha256=65a3e34fb2126f55b34e1edfc709121660e1be2dee6bdf405fc399a63a95a87d ;; \
            *) echo "no go pin for ${arch}" >&2; exit 1 ;; \
          esac; \
          archive="go${go_version}.${go_platform}.tar.gz"; \
          curl -fsSL --proto '=https' --tlsv1.2 \
            -o "/tmp/${archive}" "https://go.dev/dl/${archive}"; \
          printf '%s  /tmp/%s\n' "${go_sha256}" "${archive}" | sha256sum --check --strict -; \
          tar -xzf "/tmp/${archive}" -C /usr/local; \
          rm -f "/tmp/${archive}"; \
          ln -sf /usr/local/go/bin/go /usr/local/bin/go; \
          ln -sf /usr/local/go/bin/gofmt /usr/local/bin/gofmt; \
          go version; \
          ;; \
        jvm) \
          # jvm: Debian OpenJDK 17 headless (the LTS bookworm carries; no
          # newer JDK exists in this snapshot's main or backports) and Maven.
          apt-get -o Acquire::Check-Valid-Until=false update; \
          apt-get install -y --no-install-recommends \
            openjdk-17-jdk-headless=17.0.20+8-1~deb12u1 \
            maven=3.8.7-1; \
          rm -rf /var/lib/apt/lists/*; \
          java -version; \
          mvn --version; \
          ;; \
        *) \
          echo "unknown toolchain bundle: ${bundle}" >&2; \
          exit 1; \
          ;; \
      esac; \
    done

# The self-host profile. `TIDEBREAK_DATABASE_URL` and `TIDEBREAK_AUTH_TOKENS_FILE`
# have no defaults and must be supplied — the server refuses to open a shared
# store without a principal-naming authenticator, and refuses to start when
# that authenticator names no administrator.
ENV TIDEBREAK_PROFILE=self_host
# Instance lock, logs, and per-turn scratch live here; back this with a volume
# only if you want the logs to outlive the container. The durable state is in
# PostgreSQL.
ENV TIDEBREAK_DATA_DIR=/var/lib/tidebreak
# A writable home, on the data volume. The image's own user is below, but a
# hosting plane may run the container as any non-root uid (Model Gateway
# uses 65532), and a uid with no passwd entry is handed `HOME=/`, which
# nothing can write: npm then fails before it fetches a byte, and every
# harness install with it. The server creates this directory at boot.
ENV HOME=/var/lib/tidebreak/home
# All interfaces *of this container* — which is what makes it reachable at a
# known port from outside its own network namespace. Exposure beyond the
# container is the host's decision: docker-compose.yml publishes this to
# 127.0.0.1 only, and production exposure belongs to a TLS-terminating proxy in
# front of it. Plain HTTP by design; nothing here terminates TLS.
ENV TIDEBREAK_LISTEN_ADDR=0.0.0.0:8080
# Where the rust bundle's toolchains live when the image carries one. The
# rustup proxies under /usr/local/bin read this at run time; without it
# they look in $HOME/.rustup and find no toolchain. CARGO_HOME stays at its
# default under $HOME, which sits on the data volume and is writable, so
# the registry cache and built crates land there. Harmless when the rust
# bundle was not built in.
ENV RUSTUP_HOME=/usr/local/rustup
EXPOSE 8080
# The renderer bundle the server serves to browsers: a page navigation to any
# path the API does not own answers with it, and an unknown route fetched as
# JSON stays a 404. The server refuses to start if this names no bundle.
ENV TIDEBREAK_UI_DIST=/opt/tidebreak/ui

USER tidebreak:tidebreak

HEALTHCHECK --interval=30s --timeout=5s --start-period=90s --retries=5 \
    CMD curl -fsS http://127.0.0.1:8080/healthz || exit 1

# The entrypoint publishes the image's managed Node runtime into the data
# directory before starting the server; see entrypoint.sh for why that cannot
# happen at build time. Arguments still append to `serve`, and overriding
# `--entrypoint` reaches the binary directly.
ENTRYPOINT ["/usr/local/bin/tidebreak-entrypoint"]
