fn main() {
    let target = std::env::var("TARGET").expect("Cargo did not provide TARGET");
    let extension = if target.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let sidecar = format!("binaries/openwave-host-broker-{target}{extension}");
    println!("cargo:rerun-if-changed={sidecar}");
    println!("cargo:rerun-if-changed=../../scripts/exec-documents");

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
