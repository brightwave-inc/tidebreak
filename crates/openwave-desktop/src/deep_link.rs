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
//! itself: it is *registered* as a pending pairing
//! ([`openwave_server::register_pending_pairing`]), and the in-app sign-in
//! gate presents it. The consent is the sign-in: only a browser sign-in the
//! user completes against that gateway commits the provision, so a drive-by
//! link can at most raise a sign-in screen the user ignores. A link that
//! conflicts with the gateway already provisioned escalates instead of
//! refusing outright: a native confirmation names both origins, and only an
//! explicit confirmation parks the *replacing* pairing
//! ([`openwave_server::register_replacing_pairing`]) — the sign-in against
//! the new gateway is still the commit, so a drive-by link on a managed
//! device gets at most one dialog that defaults to changing nothing. An
//! OS (MDM) assertion is never replaceable this way. Registration and the
//! commit both live server-side, called directly on the embedded server's
//! handles, so no HTTP route — authenticated or otherwise — can reach the
//! policy write path. The webview cannot reach it either: the main window's
//! capability denies `core:event:emit`, so a compromised renderer cannot
//! forge the plugin's open-URL event; its influence stops at completing or
//! dismissing the sign-in it can already perform.

use tauri::Manager;
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tokio::sync::{oneshot, watch};

use openwave_server::{PairingError, PairingHandle, PendingRegistration};

/// Where the pairing handler waits for the embedded server's pairing handle:
/// filled once boot has bound the server. A provision link commonly
/// *launches* the app, so the handler awaits this instead of racing the boot
/// task. The handle rather than the store, because a registered pairing has
/// to land in the running server's pending slot.
pub(crate) struct PairingStore {
    rx: watch::Receiver<Option<PairingHandle>>,
}

impl PairingStore {
    pub(crate) fn new(rx: watch::Receiver<Option<PairingHandle>>) -> Self {
        Self { rx }
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
/// link surfaces the window and registers the pending pairing; a malformed
/// link is logged bounded — never echoing the link — and changes nothing.
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
    tauri::async_runtime::spawn(async move {
        let origin = link.origin.clone();
        let handle = match wait_pairing_handle(&app).await {
            Ok(handle) => handle,
            Err(reason) => {
                log_pairing(&app, &format!("pairing failed for {origin}: {reason}"));
                return;
            }
        };
        // Registration writes nothing durable and probes nothing; the
        // sign-in gate presents the pairing on its next policy poll, and a
        // sign-in the user completes there is what commits it. Only a
        // conflict with the provisioned gateway gets a native question, and
        // only the refusals get a native error — with no dialog in the
        // happy path, a refusal that reached only the log would read as the
        // app silently ignoring the link.
        match openwave_server::register_pending_pairing(&handle, &link.gateway_url).await {
            Ok(PendingRegistration::Registered) => {
                log_pairing(&app, &format!("pairing with {origin} awaits sign-in"));
            }
            Ok(PendingRegistration::AlreadyManaged) => {
                log_pairing(&app, &format!("{origin} already manages this device"));
            }
            Err(PairingError::Conflict {
                provisioned_url,
                replaceable: true,
            }) => {
                log_pairing(
                    &app,
                    &format!(
                        "pairing with {origin} conflicts with {}; asking whether to re-pair",
                        origin_of(&provisioned_url)
                    ),
                );
                confirm_and_replace(&app, &handle, &link, &provisioned_url).await;
            }
            Err(failure) => {
                log_pairing(&app, &format!("pairing refused for {origin}: {failure}"));
                show_pairing_failure(&app, &origin, &failure);
            }
        }
    });
}

/// Escalate a replaceable conflict to the user's explicit choice: a native
/// dialog naming both origins, defaulting to changing nothing. Confirmation
/// parks the replacing pairing — still nothing durable; the sign-in the gate
/// then presents is the commit. A registration the confirmation raced (the
/// provisioned row moved in between) surfaces as one failure dialog, never a
/// retry loop: the link can simply be opened again against the new state.
async fn confirm_and_replace(
    app: &tauri::AppHandle,
    handle: &PairingHandle,
    link: &ProvisionLink,
    provisioned_url: &str,
) {
    if PAIRING_DIALOG_SHOWING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        log_pairing(app, "a pairing dialog is already up; logged only");
        return;
    }
    let approved = ask_to_repair(
        app,
        &repair_prompt(&link.origin, &origin_of(provisioned_url)),
    )
    .await;
    PAIRING_DIALOG_SHOWING.store(false, std::sync::atomic::Ordering::SeqCst);
    if !approved {
        log_pairing(app, &format!("re-pairing with {} declined", link.origin));
        return;
    }
    match openwave_server::register_replacing_pairing(handle, &link.gateway_url, provisioned_url)
        .await
    {
        Ok(PendingRegistration::Registered) => {
            log_pairing(
                app,
                &format!("re-pairing with {} awaits sign-in", link.origin),
            );
        }
        Ok(PendingRegistration::AlreadyManaged) => {
            log_pairing(app, &format!("{} already manages this device", link.origin));
        }
        Err(failure) => {
            log_pairing(
                app,
                &format!("re-pairing refused for {}: {failure}", link.origin),
            );
            show_pairing_failure(app, &link.origin, &failure);
        }
    }
}

/// The confirmation itself, non-blocking like the folder-attachment prompt:
/// the dialog's answer arrives on a callback, and a channel that drops
/// unanswered reads as a decline — the failure direction of every path here
/// is "change nothing".
async fn ask_to_repair(app: &tauri::AppHandle, message: &str) -> bool {
    let (tx, rx) = oneshot::channel();
    let mut dialog = app
        .dialog()
        .message(message)
        .title("Re-pair this device?")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Re-pair".to_owned(),
            "Cancel".to_owned(),
        ));
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.parent(&window);
    }
    dialog.show(move |approved| {
        let _ = tx.send(approved);
    });
    rx.await.unwrap_or(false)
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

/// The re-pair question, pure for the same reason as [`refusal_message`]:
/// what the confirmation claims — both origins, and what confirming does —
/// is the load-bearing part, and it must be testable without a GUI.
fn repair_prompt(new_origin: &str, old_origin: &str) -> String {
    format!(
        "This device is managed by {old_origin}. Replace it with {new_origin}?\n\n\
         OpenWave will sign out of {old_origin}, and {new_origin} will control \
         which models and settings are available. You'll complete a sign-in to \
         {new_origin} to finish — until then, nothing changes."
    )
}

/// The one user-facing line per refusal class, pure so the choice of what a
/// refusal says is testable without a GUI. An MDM-asserted conflict names
/// the gateway that actually manages this device and points at the
/// administrator — that tier is never replaceable locally. A replaceable
/// conflict only reaches this dialog when the confirmed re-pair raced a
/// change to the provisioned row, so it says the state moved and leaves the
/// retry to the user's next link click. Any other refusal names only the
/// link's origin — the raw reason stays in `pairing.log`.
fn refusal_message(origin: &str, failure: &PairingError) -> String {
    match failure {
        PairingError::Conflict {
            provisioned_url,
            replaceable: false,
        } => format!(
            "This device is already managed by {}. Contact \
             your administrator to change gateways.",
            origin_of(provisioned_url)
        ),
        PairingError::Conflict { .. } => "The gateway managing this device changed while \
             re-pairing. Nothing was changed; open the pairing link again to retry."
            .to_string(),
        _ => format!(
            "OpenWave could not accept the pairing link for {origin}. This \
             device was not paired; details are in pairing.log."
        ),
    }
}

/// At most one pairing dialog — question or refusal — is up at a time. A
/// deep link is an unauthenticated remote trigger, and a page firing links
/// in a loop must not stack dialogs; the extras get the log line only.
static PAIRING_DIALOG_SHOWING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// One bounded error dialog per refused link. The happy path shows no
/// native dialog at all — the sign-in gate is the surface — so a refusal
/// that reached only the log would read as the app silently ignoring the
/// link the user just clicked. No retry affordance, no loop: the dialog
/// closes and the attempt is over.
fn show_pairing_failure(app: &tauri::AppHandle, origin: &str, failure: &PairingError) {
    if PAIRING_DIALOG_SHOWING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        log_pairing(app, "a pairing dialog is already up; logged only");
        return;
    }
    app.dialog()
        .message(refusal_message(origin, failure))
        .title("Pairing refused")
        .kind(MessageDialogKind::Error)
        .blocking_show();
    PAIRING_DIALOG_SHOWING.store(false, std::sync::atomic::Ordering::SeqCst);
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
    use super::{origin_of, provision_link, refusal_message, repair_prompt, PairingError};

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

    /// The refusal dialog's one line per failure class: an MDM-asserted
    /// conflict names the gateway that actually manages this device (never
    /// the link's) — reduced to its origin — and points at the
    /// administrator; a raced replaceable conflict says the state moved and
    /// offers no automatic retry; a generic failure names only the link's
    /// origin, keeping the raw reason out of the dialog.
    #[test]
    fn the_refusal_dialog_names_the_right_gateway() {
        let conflict = refusal_message(
            "https://new.example",
            &PairingError::Conflict {
                provisioned_url: "https://old.example/base/".to_string(),
                replaceable: false,
            },
        );
        assert!(conflict.contains("already managed by https://old.example"));
        assert!(conflict.contains("administrator"));
        assert!(!conflict.contains("new.example"));
        assert!(!conflict.contains("/base"));

        let raced = refusal_message(
            "https://new.example",
            &PairingError::Conflict {
                provisioned_url: "https://third.example/".to_string(),
                replaceable: true,
            },
        );
        assert!(raced.contains("changed while"));
        assert!(
            !raced.contains("administrator"),
            "a user-replaceable row must not be blamed on an administrator"
        );

        let other = refusal_message(
            "https://new.example",
            &PairingError::Other(openwave_core::AgentError::config(
                "reader failed: token=shh",
            )),
        );
        assert!(other.contains("https://new.example"));
        assert!(!other.contains("token=shh"));
    }

    /// The re-pair confirmation's load-bearing claims: both origins by name,
    /// which one currently manages the device, and that nothing changes
    /// before the finishing sign-in.
    #[test]
    fn the_repair_prompt_names_both_origins() {
        let prompt = repair_prompt("https://new.example", "https://old.example");
        assert!(prompt.contains("managed by https://old.example"));
        assert!(prompt.contains("Replace it with https://new.example?"));
        assert!(prompt.contains("sign out of https://old.example"));
        assert!(prompt.contains("nothing changes"));
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
