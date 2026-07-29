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
//! A deep link is an unauthenticated remote trigger — any page the user
//! visits can raise one — so a valid provision link never writes anything by
//! itself: a native dialog names the gateway origin and the consequence, and
//! only an explicit confirmation runs the pairing. Provisioning itself lives
//! server-side ([`openwave_server::pair_with_gateway`]): the shell calls it
//! directly on the embedded server's store, so no HTTP route — authenticated
//! or otherwise — can reach the policy write path. The webview cannot reach
//! it either: the main window's capability denies `core:event:emit`, so a
//! compromised renderer cannot forge the plugin's open-URL event.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::Manager;
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tokio::sync::watch;

use openwave_server::PairingHandle;

/// Where the pairing handler waits for the embedded server's pairing handle:
/// filled once boot has bound the server. A provision link commonly
/// *launches* the app, so the handler awaits this instead of racing the boot
/// task. The handle rather than the store, because pairing also applies the
/// policy it writes to the running process.
pub(crate) struct PairingStore {
    rx: watch::Receiver<Option<PairingHandle>>,
    /// One pairing at a time. While a confirmation dialog is up or a probe
    /// is in flight, further provision links are dropped with a log line
    /// instead of stacking dialogs and long-timeout probes.
    in_flight: AtomicBool,
}

impl PairingStore {
    pub(crate) fn new(rx: watch::Receiver<Option<PairingHandle>>) -> Self {
        Self {
            rx,
            in_flight: AtomicBool::new(false),
        }
    }
}

/// A validated provision link: the gateway URL to pair with, and its origin
/// — the only form of it that user-facing text and logs ever carry.
#[derive(Debug, PartialEq, Eq)]
struct ProvisionLink {
    gateway_url: String,
    origin: String,
}

/// Register the open-URL listener and pick up a launch link.
pub(crate) fn install(app: &tauri::AppHandle) {
    // Dev builds have no installer run to write the scheme registration, so
    // register at runtime. Debug-only: in a release build this would write
    // a per-user registration for whatever path the binary runs from,
    // shadowing the installer's registration. Not available on macOS, where
    // the bundle's Info.plist (generated from the deep-link config) is the
    // only registration path.
    #[cfg(all(debug_assertions, any(windows, target_os = "linux")))]
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

/// Handle every URL in one delivery. Only a link that parses as a provision
/// link surfaces the window and starts the (confirmation-gated) pairing; a
/// malformed link is logged bounded — never echoing the link — and changes
/// nothing.
fn handle_deep_link_urls(app: &tauri::AppHandle, urls: &[tauri::Url]) {
    for url in urls {
        match provision_link(url) {
            Ok(link) => {
                focus_main_window(app);
                spawn_pairing(app.clone(), link);
            }
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

/// Parse and validate a provision link.
///
/// The contract is strict — scheme `openwave`, action `provision` with no
/// userinfo, port, or extra path, exactly one query parameter named
/// `gateway`, and a gateway value that is an http(s) URL with no query or
/// fragment — so a malformed or hostile link is refused whole rather than
/// partially honored. Near-miss gateway values matter: the conflict check on
/// the write path compares normalized URL strings, so accepting a decorated
/// variant would mint a distinct, permanently conflicting policy value. The
/// gateway URL is additionally held to the connectors contract server-side.
fn provision_link(url: &tauri::Url) -> Result<ProvisionLink, String> {
    if url.scheme() != "openwave" {
        return Err("not an openwave:// link".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("the link must not carry credentials".into());
    }
    if url.port().is_some() {
        return Err("the link must not carry a port".into());
    }
    // `openwave://provision` parses the action as a host; a bare
    // `openwave:provision` parses it as the path. Anything left over in the
    // other position means a different link shape. The host is opaque for a
    // custom scheme, so a percent-encoded spelling does not match.
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
    let gateway =
        tauri::Url::parse(&value).map_err(|_| "the gateway URL is invalid".to_string())?;
    if !matches!(gateway.scheme(), "http" | "https") {
        return Err("the gateway URL must use http or https".into());
    }
    if gateway.query().is_some() || gateway.fragment().is_some() {
        return Err("the gateway URL must not carry a query or fragment".into());
    }
    Ok(ProvisionLink {
        origin: gateway.origin().ascii_serialization(),
        gateway_url: value.into_owned(),
    })
}

fn spawn_pairing(app: tauri::AppHandle, link: ProvisionLink) {
    if app
        .state::<PairingStore>()
        .in_flight
        .swap(true, Ordering::SeqCst)
    {
        log_pairing(
            &app,
            "a pairing is already awaiting confirmation; ignored another provision link",
        );
        return;
    }
    tauri::async_runtime::spawn(async move {
        let origin = link.origin.clone();
        let confirm_app = app.clone();
        let pair_app = app.clone();
        let outcome = pair_after_confirmation(
            link,
            move |origin| confirm_pairing(&confirm_app, origin),
            move |gateway_url| pair(pair_app, gateway_url),
        )
        .await;
        match outcome {
            Ok(Some(newly_provisioned)) => {
                log_pairing(&app, &format!("provisioned to {origin}"));
                if newly_provisioned {
                    prompt_restart(&app);
                }
            }
            Ok(None) => log_pairing(&app, &format!("pairing with {origin} declined")),
            Err(reason) => log_pairing(&app, &format!("pairing failed: {reason}")),
        }
        app.state::<PairingStore>()
            .in_flight
            .store(false, Ordering::SeqCst);
    });
}

/// The confirmation gate, separated from the dialog and the store so the
/// decision path is testable without a GUI: `confirm` sees only the gateway
/// origin, and nothing runs `pair` but a confirming answer. What a confirmed
/// pairing yields flows back to the caller — today, whether this pairing
/// newly provisioned the profile, which decides the restart prompt.
async fn pair_after_confirmation<C, P, F, T>(
    link: ProvisionLink,
    confirm: C,
    pair: P,
) -> Result<Option<T>, String>
where
    C: FnOnce(&str) -> bool,
    P: FnOnce(String) -> F,
    F: std::future::Future<Output = Result<T, String>>,
{
    if !confirm(&link.origin) {
        return Ok(None);
    }
    pair(link.gateway_url).await.map(Some)
}

/// Ask the user — in a native dialog, before anything is probed or written —
/// whether this device should become managed by `origin`. Blocking is fine
/// here: the pairing task runs on the async runtime's worker pool, never the
/// main thread.
fn confirm_pairing(app: &tauri::AppHandle, origin: &str) -> bool {
    app.dialog()
        .message(format!(
            "Pair OpenWave with {origin}?\n\nThis device will become managed by that gateway: \
             it will control which models are available, and the pairing cannot be undone from \
             within OpenWave."
        ))
        .title("Pair with a model gateway")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Pair".to_string(),
            "Cancel".to_string(),
        ))
        .blocking_show()
}

/// Validate, probe, and provision — all server-side. The sign-in gate is a
/// separate surface: once policy flips to managed it presents itself on its
/// next poll, so pairing does not drive the renderer. Reports whether this
/// pairing newly provisioned the profile — the restart-prompt signal.
async fn pair(app: tauri::AppHandle, gateway_url: String) -> Result<bool, String> {
    let handle = wait_pairing_handle(&app).await?;
    openwave_server::pair_with_gateway(&handle, &gateway_url)
        .await
        .map(|outcome| outcome.newly_provisioned)
        .map_err(|error| error.to_string())
}

/// Offer the restart that completes enforcement, after the pairing that
/// provisioned this profile. The embeddings client is boot-scoped (the
/// vector index is dimension-bound to it — see `resolve_embedder` in
/// `openwave-server`), so a BYOK embedder resolved at launch keeps serving
/// until the next start. An idempotent re-pair never reaches here; the
/// first pairing of a profile already OS-managed at boot does, and for it
/// the offered restart simply changes nothing. Declining is honored without
/// nagging, but not silently: one log line records that enforcement
/// completes at the next launch.
fn prompt_restart(app: &tauri::AppHandle) {
    let restart = app
        .dialog()
        .message(
            "Pairing complete — restart OpenWave to finish applying managed \
             enforcement.\n\nUntil the next launch, document embeddings keep \
             the configuration the app started with.",
        )
        .title("Pairing complete")
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Restart Now".to_string(),
            "Later".to_string(),
        ))
        .blocking_show();
    if restart {
        app.restart();
    }
    log_pairing(
        app,
        "restart deferred; managed enforcement completes at the next launch",
    );
}

async fn wait_pairing_handle(app: &tauri::AppHandle) -> Result<PairingHandle, String> {
    let mut rx = app.state::<PairingStore>().rx.clone();
    loop {
        if let Some(handle) = rx.borrow().clone() {
            return Ok(handle);
        }
        rx.changed()
            .await
            .map_err(|_| "the embedded server did not start".to_string())?;
    }
}

/// One bounded, secret-free line per pairing attempt: stderr for terminal
/// launches, `pairing.log` under app-data for GUI launches. Only the gateway
/// origin is ever named — never the full link or URL — and gateway errors
/// already strip URLs and token material.
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
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{pair_after_confirmation, provision_link, ProvisionLink};

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
            // trailing path, foreign scheme: refused whole.
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
            // Hostile decoration of the link itself: userinfo, an explicit
            // port, a percent-encoded action host.
            (
                "openwave://user:pw@provision?gateway=https://gw.example",
                None,
            ),
            ("openwave://provision:9999?gateway=https://gw.example", None),
            ("openwave://pro%76ision?gateway=https://gw.example", None),
            // Near-miss gateway values: not a URL, wrong scheme, or carrying
            // a query or fragment (which would also leak into any log that
            // echoed them, and would mint a permanently conflicting policy
            // value under the string-equality conflict check).
            ("openwave://provision?gateway=notaurl", None),
            ("openwave://provision?gateway=ftp://gw.example", None),
            (
                "openwave://provision?gateway=https://gw.example/?token=x",
                None,
            ),
            (
                "openwave://provision?gateway=https%3A%2F%2Fgw.example%2F%23frag",
                None,
            ),
        ];
        for (link, expected) in cases {
            let parsed = tauri::Url::parse(link)
                .ok()
                .and_then(|url| provision_link(&url).ok());
            assert_eq!(
                parsed.as_ref().map(|link| link.gateway_url.as_str()),
                *expected,
                "{link}"
            );
        }
    }

    #[tokio::test]
    async fn only_a_confirmed_pairing_reaches_the_write_path() {
        let link = || ProvisionLink {
            gateway_url: "https://gw.example".to_string(),
            origin: "https://gw.example".to_string(),
        };

        // Declined: the pairing action is never invoked.
        let paired = AtomicBool::new(false);
        let outcome = pair_after_confirmation(
            link(),
            |_| false,
            |_| {
                paired.store(true, Ordering::SeqCst);
                async { Ok(true) }
            },
        )
        .await;
        assert_eq!(outcome, Ok(None));
        assert!(!paired.load(Ordering::SeqCst));

        // Confirmed: the dialog sees the origin, the pairing action the URL,
        // and what the pairing yielded (the restart-prompt signal) comes
        // back to the caller — yielding `false` here, against the declined
        // arm's `true`, pins that the value is read rather than assumed.
        let outcome = pair_after_confirmation(
            link(),
            |origin| {
                assert_eq!(origin, "https://gw.example");
                true
            },
            |gateway_url| async move {
                assert_eq!(gateway_url, "https://gw.example");
                Ok(false)
            },
        )
        .await;
        assert_eq!(outcome, Ok(Some(false)));
    }
}
