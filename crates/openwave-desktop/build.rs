use std::path::{Path, PathBuf};

fn main() {
    let target = std::env::var("TARGET").expect("Cargo did not provide TARGET");
    let extension = if target.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let sidecar = format!("binaries/openwave-host-broker-{target}{extension}");
    println!("cargo:rerun-if-changed={sidecar}");

    // A packaged desktop app has no build-time PDFium cache to fall back on, so
    // stage the shared library the liteparse parser loads into `resources/` for
    // the bundler to ship. Only release builds are packaged; dev keeps resolving
    // PDFium via the compile-time cache path pdfium-sys bakes in.
    if std::env::var("PROFILE").as_deref() == Ok("release") {
        stage_pdfium_runtime();
    }

    // Plain `cargo check` and `cargo test` do not run Tauri's before-build
    // hook. Keep those workflows usable from a clean checkout; Tauri dev/build
    // runs prepare-sidecar first, so the default config always packages the
    // real target-specific executable.
    if !std::path::Path::new(&sidecar).is_file() {
        if std::env::var("PROFILE").as_deref() == Ok("release") {
            panic!(
                "release sidecar is missing at {sidecar}; build the desktop through `cargo tauri build`"
            );
        }
        let mut overlay = std::env::var("TAURI_CONFIG")
            .ok()
            .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        overlay["bundle"]["externalBin"] = serde_json::Value::Null;
        let overlay = overlay.to_string();
        std::env::set_var("TAURI_CONFIG", &overlay);
        println!("cargo:rustc-env=TAURI_CONFIG={overlay}");
    }
    tauri_build::build()
}

/// Platform file name of the PDFium shared library, keyed off the build target.
fn pdfium_dylib_name() -> &'static str {
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("windows") => "pdfium.dll",
        Ok("macos") => "libpdfium.dylib",
        _ => "libpdfium.so",
    }
}

/// Copy the PDFium shared library into `resources/pdfium/` so `tauri.conf.json`'s
/// `bundle.resources` glob ships it next to the packaged app.
///
/// `liteparse-pdfium-sys`'s own build script copies the resolved library into
/// this build's `target/<profile>/deps/`; because the desktop crate depends on
/// it transitively, that copy has already happened by the time this runs. We
/// read it back from there so the staged binary is exactly the pinned version
/// the app was compiled against — never a stray system copy. A missing library
/// is a loud warning rather than a hard error: the bundle still builds, and the
/// packaged app fails closed on PDF parsing with a clear message until a human
/// confirms the runtime is present.
fn stage_pdfium_runtime() {
    let name = pdfium_dylib_name();

    let Some(source) = pdfium_deps_path(name) else {
        println!(
            "cargo:warning=PDFium runtime not found in target/deps; the packaged app will not \
             parse PDFs. Build the desktop through `cargo tauri build` so pdfium-sys stages the \
             library first."
        );
        return;
    };

    let dest_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("resources")
        .join("pdfium");
    if let Err(error) = std::fs::create_dir_all(&dest_dir) {
        println!(
            "cargo:warning=could not create {}: {error}",
            dest_dir.display()
        );
        return;
    }
    let dest = dest_dir.join(name);
    if let Err(error) = std::fs::copy(&source, &dest) {
        println!(
            "cargo:warning=could not stage PDFium runtime from {} to {}: {error}",
            source.display(),
            dest.display()
        );
    }
    println!("cargo:rerun-if-changed={}", source.display());
}

/// Locate the PDFium library `pdfium-sys` copied into this build's `deps/` dir.
///
/// `OUT_DIR` is `target/[<triple>/]<profile>/build/<pkg>-<hash>/out`; the shared
/// `deps/` sibling lives three levels up.
fn pdfium_deps_path(name: &str) -> Option<PathBuf> {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").ok()?);
    let deps = out_dir.parent()?.parent()?.parent()?.join("deps");
    let candidate = deps.join(name);
    candidate.is_file().then_some(candidate)
}
