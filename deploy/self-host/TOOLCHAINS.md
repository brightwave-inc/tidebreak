# Self-host toolchain bundle pins

The `TOOLCHAINS` build argument on `Dockerfile` installs these optional
bundles. Bump a pin here and in the Dockerfile together. Empty
`TOOLCHAINS` installs none of them.

| Bundle | Artifact | Pin |
| --- | --- | --- |
| `rust` | rustup-init | 1.27.1 |
| `rust` | rustup-init linux x86_64 SHA-256 | `6aeece6993e902708983b209d04c0d1dbb13ebb71cc85f9723c58632ddd0736b` |
| `rust` | rustup-init linux aarch64 SHA-256 | `1cffbf0f934dc9de73c67bb9255e25df11c82c2acc5d00f9699371d677138b85` |
| `rust` | rustc/cargo/clippy/rustfmt toolchain | 1.97.1 (workspace `rust-toolchain.toml`) |
| `rust` | Debian `build-essential` | 12.9 |
| `python` | Debian `python3` | 3.11.2-6+deb12u6 |
| `python` | Debian `python3-pip` | 23.0.1+dfsg-1 |
| `python` | Debian `python3-venv` | 3.11.2-6+deb12u6 |
| `go` | Go release | 1.25.1 |
| `go` | `go1.25.1.linux-amd64.tar.gz` SHA-256 | `7716a0d940a0f6ae8e1f3b3f4f36299dc53e31b16840dbd410459de654d1b124` |
| `go` | `go1.25.1.linux-arm64.tar.gz` SHA-256 | `65a3e34fbdc6cf5e95d66d127dc05c716f818e3c0bf240549d84399babd47f38` |
| `jvm` | Debian `openjdk-21-jdk-headless` (bookworm-backports, snapshot 20260810T000000Z) | 21.0.8+9-1~deb12u1~bpo12+1 |
| `jvm` | Debian `maven` | 3.8.7-1 |

The image label `io.tidebreak.toolchains` is set from the `TOOLCHAINS`
argument so a digest's provenance lists the bundles it was built with.
