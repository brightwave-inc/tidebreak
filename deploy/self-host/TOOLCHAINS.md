# Self-host toolchain bundle pins

The `TOOLCHAINS` build argument on `Dockerfile` installs these optional
bundles. Bump a pin here and in the Dockerfile together. Empty
`TOOLCHAINS` installs none of them.

| Bundle | Artifact | Pin |
| --- | --- | --- |
| `rust` | rustup-init | 1.27.1 |
| `rust` | rustup-init linux x86_64 SHA-256 | `6aeece6993e902708983b209d04c0d1dbb14ebb405ddb87def578d41f920f56d` |
| `rust` | rustup-init linux aarch64 SHA-256 | `1cffbf51e63e634c746f741de50649bbbcbd9dbe1de363c9ecef64e278dba2b2` |
| `rust` | rustc/cargo/clippy/rustfmt toolchain | 1.97.1 (workspace `rust-toolchain.toml`) |
| `rust` | Debian `build-essential` | 12.9 |
| `python` | Debian `python3` | 3.11.2-1+b1 |
| `python` | Debian `python3-pip` | 23.0.1+dfsg-1 |
| `python` | Debian `python3-venv` | 3.11.2-1+b1 |
| `go` | Go release | 1.25.1 |
| `go` | `go1.25.1.linux-amd64.tar.gz` SHA-256 | `7716a0d940a0f6ae8e1f3b3f4f36299dc53e31b16840dbd171254312c41ca12e` |
| `go` | `go1.25.1.linux-arm64.tar.gz` SHA-256 | `65a3e34fb2126f55b34e1edfc709121660e1be2dee6bdf405fc399a63a95a87d` |
| `jvm` | Debian `openjdk-17-jdk-headless` (bookworm-security, snapshot 20260810T000000Z) | 17.0.20+8-1~deb12u1 |
| `jvm` | Debian `maven` | 3.8.7-1 |

The image label `io.tidebreak.toolchains` is set from the `TOOLCHAINS`
argument so a digest's provenance lists the bundles it was built with.
