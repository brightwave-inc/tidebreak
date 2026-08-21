fn main() {
    let target = std::env::var("TARGET").expect("Cargo did not provide TARGET");
    let extension = if target.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let host_broker = format!("binaries/tidebreak-host-broker-{target}{extension}");
    let cli = format!("binaries/tidebreak-{target}{extension}");

    println!("cargo:rerun-if-changed={host_broker}");
    println!("cargo:rerun-if-changed={cli}");
    println!("cargo:rerun-if-changed=../../scripts/exec-documents");

    println!("cargo:rerun-if-env-changed=TIDEBREAK_CHANNEL");
    if let Ok(channel) = std::env::var("TIDEBREAK_CHANNEL") {
        if !channel.is_empty() {
            println!("cargo:rustc-env=TIDEBREAK_CHANNEL={channel}");
        }
    }

    let release = std::env::var("PROFILE").as_deref() == Ok("release");
    let mut overlay = std::env::var("TAURI_CONFIG")
        .ok()
        .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let mut overlaid = false;

    // Plain `cargo check` and `cargo test` do not run Tauri's before-build
    // hook. Keep those workflows usable from a clean checkout; Tauri dev/build
    // runs prepare-sidecar first, so the default config always packages the
    // real target-specific executables.
    let host_broker_present = std::path::Path::new(&host_broker).is_file();
    let cli_present = std::path::Path::new(&cli).is_file();

    if release && (!host_broker_present || !cli_present) {
        let missing: Vec<&str> = [
            (!host_broker_present).then_some(host_broker.as_str()),
            (!cli_present).then_some(cli.as_str()),
        ]
        .into_iter()
        .flatten()
        .collect();
        panic!(
            "release sidecar(s) missing: {}; build the desktop through `cargo tauri build`",
            missing.join(", ")
        );
    }

    if !host_broker_present || !cli_present {
        overlay["bundle"]["externalBin"] = serde_json::Value::Null;
        overlaid = true;
    }

    // Debug builds embed a red icon set so a dev window is unmistakable next
    // to an installed release in the dock and app switcher.
    if !release {
        overlay["bundle"]["icon"] = serde_json::json!([
            "icons/dev/32x32.png",
            "icons/dev/128x128.png",
            "icons/dev/128x128@2x.png",
            "icons/dev/icon.icns",
            "icons/dev/icon.ico",
            "icons/dev/icon.png"
        ]);
        overlaid = true;
    }

    if overlaid {
        let overlay = overlay.to_string();
        std::env::set_var("TAURI_CONFIG", &overlay);
        println!("cargo:rustc-env=TAURI_CONFIG={overlay}");
    }
    tauri_build::build()
}
