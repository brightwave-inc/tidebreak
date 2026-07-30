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

use openwave_server::{PairingError, PairingHandle};

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

/// The scheme dev builds register and answer alongside `openwave`. A dev run
/// used to `register_all()`, which persistently re-pointed the real scheme's
/// per-user registration at whatever debug binary ran last — links on that
/// machine then bypassed the installed app until it was reinstalled. The dev
/// scheme keeps dev deep-link work exercisable without ever touching the
/// installed registration. It must stay in the deep-link config's scheme
/// list: the plugin only recognizes configured schemes when it picks deep
/// links out of launch arguments.
const DEV_SCHEME: &str = "openwave-dev";

/// Register the open-URL listener and pick up a launch link.
pub(crate) fn install(app: &tauri::AppHandle) {
    // Dev builds have no installer run to write the scheme registration, so
    // register at runtime — only the dev scheme, never the real one, which
    // in a dev run would shadow the installed app's registration. Debug-only:
    // a release binary's registration is the installer's job. Not available
    // on macOS, where the bundle's Info.plist (generated from the deep-link
    // config) is the only registration path.
    #[cfg(all(debug_assertions, any(windows, target_os = "linux")))]
    if let Err(error) = app.deep_link().register(DEV_SCHEME) {
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
/// The contract is strict — scheme `openwave` (dev builds also answer
/// `openwave-dev`), action `provision` with no userinfo, port, or extra
/// path, exactly one query parameter named `gateway`, and a gateway value
/// that is an http(s) URL with no userinfo, query, or fragment — so a
/// malformed or hostile link is refused whole rather than partially honored.
/// Near-miss gateway values matter: the conflict check on the write path
/// compares normalized URL strings, so accepting a decorated variant would
/// mint a distinct, permanently conflicting policy value. The gateway URL is
/// additionally held to the connectors contract server-side.
fn provision_link(url: &tauri::Url) -> Result<ProvisionLink, String> {
    if url.scheme() != "openwave" && !(cfg!(debug_assertions) && url.scheme() == DEV_SCHEME) {
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
    if !gateway.username().is_empty() || gateway.password().is_some() {
        return Err("the gateway URL must not carry credentials".into());
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
            Ok(Some(())) => {
                log_pairing(&app, &format!("provisioned to {origin}"));
            }
            Ok(None) => log_pairing(&app, &format!("pairing with {origin} declined")),
            Err(failure) => {
                log_pairing(&app, &format!("pairing failed for {origin}: {failure}"));
                show_pairing_failure(&app, &origin, &failure);
            }
        }
        app.state::<PairingStore>()
            .in_flight
            .store(false, Ordering::SeqCst);
    });
}

/// The confirmation gate, separated from the dialog and the store so the
/// decision path is testable without a GUI: `confirm` sees only the gateway
/// origin, and nothing runs `pair` but a confirming answer. What a confirmed
/// pairing yields flows back to the caller, as does what a failed one raised,
/// which decides the refusal dialog.
async fn pair_after_confirmation<C, P, F, T, E>(
    link: ProvisionLink,
    confirm: C,
    pair: P,
) -> Result<Option<T>, E>
where
    C: FnOnce(&str) -> bool,
    P: FnOnce(String) -> F,
    F: std::future::Future<Output = Result<T, E>>,
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

/// A pairing failure, split the way the user-facing surface needs it: the
/// conflict refusal is a product state with its own explanation, everything
/// else a generic fault whose details belong in the log, not the dialog.
enum PairFailure {
    /// This device is provisioned to a different gateway. Refuse-forever is
    /// the recorded decision, so the dialog names that gateway's origin and
    /// points at the administrator instead of offering a retry.
    Conflict { provisioned_origin: String },
    /// Anything else: unreachable gateway, invalid manifest, the embedded
    /// server not starting.
    Other(String),
}

impl std::fmt::Display for PairFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict { provisioned_origin } => write!(
                f,
                "refused: this device is already provisioned to {provisioned_origin}"
            ),
            Self::Other(reason) => f.write_str(reason),
        }
    }
}

/// Validate, probe, and provision — all server-side. The sign-in gate is a
/// separate surface: once policy flips to managed it presents itself on its
/// next poll, so pairing does not drive the renderer. Reports whether this
/// keeps the server's typed conflict refusal distinct from other failures,
/// reduced to the provisioned gateway's origin.
async fn pair(app: tauri::AppHandle, gateway_url: String) -> Result<(), PairFailure> {
    let handle = wait_pairing_handle(&app)
        .await
        .map_err(PairFailure::Other)?;
    match openwave_server::pair_with_gateway(&handle, &gateway_url).await {
        Ok(_) => Ok(()),
        Err(PairingError::Conflict { provisioned_url }) => Err(PairFailure::Conflict {
            provisioned_origin: origin_of(&provisioned_url),
        }),
        Err(error) => Err(PairFailure::Other(error.to_string())),
    }
}

/// Reduce a stored gateway base URL to its origin — the only form
/// user-facing text and logs carry. The stored URL passed the gateway
/// contract at provisioning, so this parse does not fail in practice; the
/// fallback keeps the dialog honest rather than empty if it ever does.
fn origin_of(url: &str) -> String {
    tauri::Url::parse(url)
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_else(|_| "another gateway".to_string())
}

/// The one user-facing line per failure class, pure so the choice of what a
/// refusal says is testable without a GUI. The conflict names the gateway
/// that actually manages this device and points at the administrator; any
/// other failure names only the origin the user just confirmed — the raw
/// reason stays in `pairing.log`.
fn refusal_message(origin: &str, failure: &PairFailure) -> String {
    match failure {
        PairFailure::Conflict { provisioned_origin } => format!(
            "This device is already managed by {provisioned_origin}. Contact \
             your administrator to change gateways."
        ),
        PairFailure::Other(_) => format!(
            "OpenWave could not pair with {origin}. This device was not \
             paired; details are in pairing.log."
        ),
    }
}

/// One bounded error dialog per failed attempt, for both failure classes:
/// the user just confirmed a pairing in a dialog, so a refusal that reaches
/// only the log reads as success — the exact silent confusion the
/// confirmation flow exists to avoid. No retry affordance, no loop: the
/// dialog closes and the attempt is over.
fn show_pairing_failure(app: &tauri::AppHandle, origin: &str, failure: &PairFailure) {
    app.dialog()
        .message(refusal_message(origin, failure))
        .title("Pairing failed")
        .kind(MessageDialogKind::Error)
        .blocking_show();
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

/// Growth cap for `pairing.log`. Every ignored deep link writes a line, and
/// a deep link is an unauthenticated remote trigger, so without a cap a
/// hostile page could grow the file without bound. Attempts past the cap
/// still reach stderr.
const PAIRING_LOG_MAX_BYTES: u64 = 256 * 1024;

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
    let path = dir.join("pairing.log");
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() >= PAIRING_LOG_MAX_BYTES) {
        return;
    }
    let line = format!("{} {message}\n", chrono::Local::now().to_rfc3339());
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(line.as_bytes()));
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{
        origin_of, pair_after_confirmation, provision_link, refusal_message, PairFailure,
        ProvisionLink,
    };

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
            // Near-miss gateway values: not a URL, wrong scheme, carrying
            // credentials, or carrying a query or fragment (which would also
            // leak into any log that echoed them, and would mint a
            // permanently conflicting policy value under the string-equality
            // conflict check).
            ("openwave://provision?gateway=notaurl", None),
            ("openwave://provision?gateway=ftp://gw.example", None),
            (
                "openwave://provision?gateway=https://user:pw@gw.example",
                None,
            ),
            ("openwave://provision?gateway=https://user@gw.example", None),
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

    /// Dev builds answer the dev scheme so a dev run never has to claim the
    /// real one; release builds refuse it. The expectation flips with the
    /// build profile, which is exactly the contract.
    #[test]
    fn the_dev_scheme_is_a_debug_only_alias() {
        let url = tauri::Url::parse("openwave-dev://provision?gateway=https://gw.example").unwrap();
        assert_eq!(provision_link(&url).is_ok(), cfg!(debug_assertions));
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
                async { Ok::<_, String>(true) }
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
                Ok::<_, String>(false)
            },
        )
        .await;
        assert_eq!(outcome, Ok(Some(false)));
    }

    /// The refusal dialog's one line per failure class: the conflict names
    /// the gateway that actually manages this device (never the link's) and
    /// points at the administrator; a generic failure names only the origin
    /// the user confirmed, keeping the raw reason out of the dialog.
    #[test]
    fn the_refusal_dialog_names_the_right_gateway() {
        let conflict = refusal_message(
            "https://new.example",
            &PairFailure::Conflict {
                provisioned_origin: "https://old.example".to_string(),
            },
        );
        assert!(conflict.contains("already managed by https://old.example"));
        assert!(conflict.contains("administrator"));
        assert!(!conflict.contains("new.example"));

        let other = refusal_message(
            "https://new.example",
            &PairFailure::Other("probe failed: token=shh".to_string()),
        );
        assert!(other.contains("https://new.example"));
        assert!(!other.contains("token=shh"));
    }

    /// Pins the load-bearing reduction the conflict dialog's claim rests
    /// on: scheme, host, and port — nothing else of the stored base URL.
    #[test]
    fn a_stored_base_url_reduces_to_its_origin() {
        assert_eq!(
            origin_of("https://gw.example:8443/base/path/"),
            "https://gw.example:8443"
        );
    }
}
