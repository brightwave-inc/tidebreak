//! `openwave://` deep-link handling: gateway pairing.
//!
//! One link shape is recognized: `openwave://provision?gateway=<url>`. macOS
//! delivers opened URLs through the runtime's opened-URL events, which the
//! deep-link plugin re-emits; on Windows and Linux the OS launches a second
//! app instance with the link as its only argument, and the single-instance
//! plugin (via its `deep-link` feature) forwards those arguments to the
//! deep-link plugin in the primary process. Every warm path therefore
//! funnels into the one open-URL listener registered here; a cold start on
//! Windows/Linux is picked up from the plugin's recorded launch URL right
//! after the listener is registered.
//!
//! Provisioning itself lives server-side ([`openwave_server::pair_with_gateway`]):
//! the shell calls it directly on the embedded server's store, so no HTTP
//! route — authenticated or otherwise — can reach the policy write path.

use std::sync::Arc;

use tauri::Manager;
use tauri_plugin_deep_link::DeepLinkExt;
use tokio::sync::watch;

use openwave_core::Store;

/// Where the pairing handler waits for the embedded server's store: filled
/// once boot has bound the server. A provision link commonly *launches* the
/// app, so the handler awaits this instead of racing the boot task.
pub(crate) struct PairingStore {
    rx: watch::Receiver<Option<Arc<dyn Store>>>,
}

impl PairingStore {
    pub(crate) fn new(rx: watch::Receiver<Option<Arc<dyn Store>>>) -> Self {
        Self { rx }
    }
}

/// Register the open-URL listener and pick up a launch link.
pub(crate) fn install(app: &tauri::AppHandle) {
    // Dev builds and portable installs have no installer run to write the
    // scheme registration; best-effort runtime registration keeps the link
    // working there. Not available on macOS, where the bundle's Info.plist
    // (generated from the deep-link config) is the only registration path.
    #[cfg(any(windows, target_os = "linux"))]
    if let Err(error) = app.deep_link().register_all() {
        eprintln!("openwave-desktop: deep-link scheme registration failed: {error}");
    }
    let handle = app.clone();
    app.deep_link().on_open_url(move |event| {
        handle_deep_link_urls(&handle, &event.urls());
    });
    // Windows/Linux cold start: the launch link was recorded by the plugin
    // before this listener existed. macOS delivers the launch link as an
    // opened-URL event after setup, so it reaches the listener instead.
    if let Ok(Some(urls)) = app.deep_link().get_current() {
        handle_deep_link_urls(app, &urls);
    }
}

/// Handle every `openwave://` URL in one delivery: surface the window, then
/// pair for each well-formed provision link. A malformed link is logged
/// (bounded, without echoing the link) and changes nothing.
fn handle_deep_link_urls(app: &tauri::AppHandle, urls: &[tauri::Url]) {
    for url in urls {
        if url.scheme() != "openwave" {
            continue;
        }
        focus_main_window(app);
        match provision_gateway_url(url) {
            Ok(gateway_url) => spawn_pairing(app.clone(), gateway_url),
            Err(reason) => log_pairing(app, &format!("ignored a deep link: {reason}")),
        }
    }
}

/// Bring the main window to the user: pairing was asked for from outside the
/// app (a browser or terminal), and its outcome shows in the app.
pub(crate) fn focus_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Extract the gateway URL from a provision link.
///
/// The contract is strict — scheme `openwave`, action `provision`, exactly
/// one query parameter named `gateway` — so a malformed link is refused
/// whole rather than partially honored. The gateway URL value itself is held
/// to the gateway contract server-side, before anything is written.
fn provision_gateway_url(url: &tauri::Url) -> Result<String, String> {
    if url.scheme() != "openwave" {
        return Err("not an openwave:// link".into());
    }
    // `openwave://provision` parses the action as a host; a bare
    // `openwave:provision` parses it as the path. Anything left over in the
    // other position means a different link shape.
    let action = match (url.host_str(), url.path().trim_matches('/')) {
        (Some(host), "") => host.to_string(),
        (None, path) => path.to_string(),
        (Some(host), path) => format!("{host}/{path}"),
    };
    if action != "provision" {
        return Err("not a provision link".into());
    }
    let mut pairs = url.query_pairs();
    let Some((key, value)) = pairs.next() else {
        return Err("the provision link carries no gateway parameter".into());
    };
    if pairs.next().is_some() {
        return Err("the provision link must carry exactly one parameter".into());
    }
    if key != "gateway" {
        return Err("the provision link carries an unrecognized parameter".into());
    }
    if value.is_empty() {
        return Err("the provision link carries an empty gateway URL".into());
    }
    Ok(value.into_owned())
}

fn spawn_pairing(app: tauri::AppHandle, gateway_url: String) {
    tauri::async_runtime::spawn(async move {
        match pair(&app, &gateway_url).await {
            Ok(base_url) => log_pairing(&app, &format!("provisioned to {base_url}")),
            Err(reason) => log_pairing(&app, &format!("pairing failed: {reason}")),
        }
    });
}

/// Validate, probe, and provision — all server-side. The sign-in gate is a
/// separate surface: once policy flips to managed it presents itself on its
/// next poll, so pairing does not drive the renderer.
async fn pair(app: &tauri::AppHandle, gateway_url: &str) -> Result<String, String> {
    let store = wait_store(app).await?;
    openwave_server::pair_with_gateway(&*store, gateway_url)
        .await
        .map_err(|error| error.to_string())
}

async fn wait_store(app: &tauri::AppHandle) -> Result<Arc<dyn Store>, String> {
    let mut rx = app.state::<PairingStore>().rx.clone();
    loop {
        if let Some(store) = rx.borrow().clone() {
            return Ok(store);
        }
        rx.changed()
            .await
            .map_err(|_| "the embedded server did not start".to_string())?;
    }
}

/// One bounded, secret-free line per pairing attempt: stderr for terminal
/// launches, `pairing.log` under app-data for GUI launches. Gateway errors
/// already strip URLs and token material; the raw link is never echoed.
fn log_pairing(app: &tauri::AppHandle, message: &str) {
    use std::io::Write;
    eprintln!("openwave-desktop: gateway pairing: {message}");
    let Ok(dir) = crate::data_dir(app) else {
        return;
    };
    let line = format!("{} {message}\n", chrono::Local::now().to_rfc3339());
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("pairing.log"))
        .and_then(|mut file| file.write_all(line.as_bytes()));
}

#[cfg(test)]
mod tests {
    use super::provision_gateway_url;

    #[test]
    fn provision_links_are_held_to_the_contract() {
        let cases: &[(&str, Option<&str>)] = &[
            // Well-formed, including the trailing-slash and encoded forms
            // real browsers and MDM tooling produce.
            (
                "openwave://provision?gateway=https://gw.example",
                Some("https://gw.example"),
            ),
            (
                "openwave://provision/?gateway=https%3A%2F%2Fgw.example%2F",
                Some("https://gw.example/"),
            ),
            (
                "openwave:provision?gateway=https://gw.example",
                Some("https://gw.example"),
            ),
            // Wrong action, missing/empty/duplicated/unknown parameters,
            // trailing path, foreign scheme: all refused whole.
            ("openwave://settings?gateway=https://gw.example", None),
            ("openwave://provision", None),
            ("openwave://provision?gateway=", None),
            (
                "openwave://provision?gateway=https://a.example&gateway=https://b.example",
                None,
            ),
            (
                "openwave://provision?gateway=https://gw.example&extra=1",
                None,
            ),
            (
                "openwave://provision/extra?gateway=https://gw.example",
                None,
            ),
            ("https://provision?gateway=https://gw.example", None),
        ];
        for (link, expected) in cases {
            let url = tauri::Url::parse(link).expect(link);
            assert_eq!(
                provision_gateway_url(&url).ok().as_deref(),
                *expected,
                "{link}"
            );
        }
    }
}
