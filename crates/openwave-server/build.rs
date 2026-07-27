//! Refuse to produce a release build without the durable vector store.
//!
//! `vec-lance` is off by default so ordinary builds and tests skip the LanceDB
//! dependency tree; retrieval then runs on an in-memory index that is discarded
//! when the process exits. That trade is right for a dev build and wrong for a
//! shipped one — a packaged app whose documents vanish on restart is data loss,
//! and it would look like a working build. So the release profile has to opt in
//! explicitly, and this check fails the build when it does not.
//!
//! `PROFILE` is `release` for the release profile and any profile inheriting it,
//! independent of `debug_assertions` and of any profile tuning, which is why the
//! check lives here rather than in a `cfg(not(debug_assertions))` assertion.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let release = std::env::var("PROFILE").as_deref() == Ok("release");
    let durable = std::env::var_os("CARGO_FEATURE_VEC_LANCE").is_some();
    if release && !durable {
        eprintln!(
            "openwave-server: a release build must keep the durable vector store, \
             but the `vec-lance` feature is off. Without it, document search runs \
             on an in-memory index that is lost when the process exits. Build with \
             `--features vec-lance` (openwave-cli, openwave-desktop, and \
             `cargo tauri build` all forward it), or use a dev profile."
        );
        std::process::exit(1);
    }
}
