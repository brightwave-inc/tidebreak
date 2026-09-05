//! Bounded semantic observation and action for Tidebreak-owned browser tabs.
//!
//! Page data is treated as untrusted throughout. The host retains selectors
//! and fingerprints, while agents receive only compact ephemeral refs. Every
//! action must present the exact snapshot id and document epoch that produced
//! its ref; navigation or a replaced target returns `stale_target` without an
//! action.

use std::collections::HashMap;

use base64::Engine as _;
use serde::Deserialize;
use tauri::{AppHandle, Manager, Webview};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
#[cfg(test)]
use tidebreak_core::MAX_BROWSER_SNAPSHOT_NODES;
use tidebreak_core::{
    BrowserContentTrust, BrowserElementBounds, BrowserGrantCapability, BrowserOrigin,
    BrowserPageSnapshot, BrowserSemanticFrame, BrowserSemanticNode, BrowserSemanticNodeKind,
    BrowserSnapshotArgs, BrowserViewport,
};
use tokio::sync::oneshot;
use uuid::Uuid;

#[cfg(target_os = "macos")]
use crate::browser_native_webview::with_browser_webview;

use crate::{
    browser_control::{
        BrowserConfirmationBinding, BrowserDispatchEffect, BrowserLoadState,
        BrowserObservationFence, BrowserRegistry, BrowserSnapshot, BrowserTargetError,
        BrowserTargetFingerprint, BrowserTargetRecord,
    },
    code_browser::{begin_agent_browser_control, browser_label},
};

const MAX_ACTION_VALUE_CHARS: usize = 8_192;
const JAVASCRIPT_TIMEOUT_SECONDS: u64 = 10;
const NATIVE_EVENT_PROCESSING_TIMEOUT_SECONDS: u64 = 2;
const MAX_NATIVE_SELECT_STEPS: usize = 512;
const MAX_NATIVE_SCROLL_STEPS: usize = 24;
const NATIVE_ACTION_VERIFY_DELAY_MILLIS: u64 = 25;

#[cfg(target_os = "macos")]
thread_local! {
    static BROWSER_SEMANTICS_CONTENT_WORLD: std::cell::OnceCell<
        objc2::rc::Retained<objc2_web_kit::WKContentWorld>,
    > = const { std::cell::OnceCell::new() };
}

#[cfg(target_os = "macos")]
fn browser_semantics_content_world(
) -> Result<objc2::rc::Retained<objc2_web_kit::WKContentWorld>, String> {
    use objc2::MainThreadMarker;
    use objc2_foundation::NSString;
    use objc2_web_kit::WKContentWorld;

    let Some(mtm) = MainThreadMarker::new() else {
        return Err("browser JavaScript isolation requires the main thread".to_owned());
    };
    BROWSER_SEMANTICS_CONTENT_WORLD
        .try_with(|world| {
            world
                .get_or_init(|| unsafe {
                    let name = NSString::from_str("TidebreakBrowserSemantics");
                    WKContentWorld::worldWithName(&name, mtm)
                })
                .clone()
        })
        .map_err(|_| "browser JavaScript isolation is unavailable".to_owned())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSemanticSnapshot {
    url: String,
    title: String,
    viewport: BrowserViewport,
    nodes: Vec<RawSemanticNode>,
    frames: Vec<BrowserSemanticFrame>,
    truncated: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSemanticNode {
    kind: BrowserSemanticNodeKind,
    #[serde(rename = "ref")]
    target_ref: Option<String>,
    tag: String,
    role: String,
    name: String,
    frame: String,
    text: Option<String>,
    value: Option<String>,
    href: Option<String>,
    input_type: Option<String>,
    disabled: bool,
    checked: Option<bool>,
    sensitive: bool,
    consequential: bool,
    actions: Vec<String>,
    bounds: BrowserElementBounds,
    selector: Option<String>,
    frame_path: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum NativeActionResolutionStatus {
    Ready,
    NoOp,
    PendingNativeInput,
    StaleTarget,
    HumanTakeoverRequired,
    UnsupportedFrame,
    UnsupportedNative,
    InvalidValue,
    TargetObscured,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeActionResolution {
    status: NativeActionResolutionStatus,
    message: String,
    url: String,
    title: String,
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
    #[serde(default)]
    width: Option<f64>,
    #[serde(default)]
    height: Option<f64>,
    #[serde(default)]
    viewport_width: Option<f64>,
    #[serde(default)]
    viewport_height: Option<f64>,
    #[serde(default)]
    option_index: Option<i64>,
    #[serde(default)]
    selected_index: Option<i64>,
    #[serde(default)]
    scroll_x: Option<f64>,
    #[serde(default)]
    scroll_y: Option<f64>,
    #[serde(default)]
    scroll_delta_x: Option<f64>,
    #[serde(default)]
    scroll_delta_y: Option<f64>,
    #[serde(default)]
    target_focused: bool,
    #[serde(default)]
    target_dom_focused: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawBrowserUploadResult {
    status: tidebreak_core::BrowserUploadStatus,
    message: String,
}

/// Exact file bytes resolved by the foreground native executor.
///
/// This value never crosses renderer IPC. The isolated WebKit evaluation
/// receives it only while attaching the confirmed file to the target page.
pub(crate) struct BrowserUploadFile {
    pub(crate) filename: String,
    pub(crate) media_type: String,
    pub(crate) bytes: Vec<u8>,
    pub(crate) binding: BrowserConfirmationBinding,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawScreenshotPrivacyScan {
    sensitive_fields: usize,
    uninspectable_regions: usize,
    changed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTextProbe {
    contains: bool,
    uninspectable_regions: usize,
}

pub(crate) async fn browser_semantic_snapshot(
    app: &AppHandle,
    registry: &BrowserRegistry,
    capability_id: Uuid,
    arguments: BrowserSnapshotArgs,
) -> Result<BrowserPageSnapshot, String> {
    if !arguments.is_well_formed() {
        return Err("browser snapshot request is not valid".to_owned());
    }
    let host_snapshot =
        begin_agent_browser_control(app, registry, capability_id, &arguments.browser_id)?;
    if host_snapshot.load_state != Some(BrowserLoadState::Ready) {
        return Err("browser page is still loading".to_owned());
    }
    let origin = host_snapshot
        .url
        .as_deref()
        .and_then(BrowserOrigin::from_url)
        .ok_or_else(|| "browser has no authorized HTTP origin".to_owned())?;
    let workspace_id = host_snapshot.workspace_id;
    let browser_id = arguments.browser_id.clone();
    let capture_browser_id = browser_id.clone();
    let max_nodes = arguments.bounded_max_nodes();
    let app = app.clone();
    let dispatch_registry = registry.clone();
    registry
        .dispatch_agent(
            capability_id,
            &browser_id,
            &origin,
            BrowserGrantCapability::BrowserObserveOrigin,
            "semantic_snapshot",
            None,
            BrowserDispatchEffect::Observe,
            None,
            move || async move {
                capture_semantic_snapshot(
                    app,
                    dispatch_registry,
                    capability_id,
                    workspace_id,
                    capture_browser_id,
                    max_nodes,
                )
                .await
            },
        )
        .await
}

async fn capture_semantic_snapshot(
    app: AppHandle,
    registry: BrowserRegistry,
    capability_id: Uuid,
    workspace_id: String,
    browser_id: String,
    max_nodes: usize,
) -> Result<BrowserPageSnapshot, String> {
    let label = browser_label(&browser_id)?;

    // Capture the instance identity and document epoch before the page
    // JavaScript evaluation so a Stop/replace cannot reuse the stale
    // result after completion.
    let start_fence = registry.observation_fence(capability_id, &browser_id)?;
    let instance_id = start_fence.instance_id;
    let document_epoch = start_fence.document_epoch;

    let host_snapshot = registry.snapshot(&browser_id, &workspace_id)?;
    if host_snapshot.load_state != Some(BrowserLoadState::Ready) {
        return Err("browser page is still loading".to_owned());
    }

    let webview = app
        .get_webview(&label)
        .ok_or_else(|| "browser session is not open".to_owned())?;

    let snapshot_id = Uuid::new_v4().to_string();
    let marker = marker_for_snapshot(&snapshot_id);
    let raw: RawSemanticSnapshot =
        evaluate_json(&webview, &snapshot_script(max_nodes, &marker)).await?;
    let mut targets = HashMap::with_capacity(raw.nodes.len());
    let mut nodes = Vec::with_capacity(raw.nodes.len());
    for node in raw.nodes {
        if node.kind == BrowserSemanticNodeKind::Interactive {
            let target_ref = node
                .target_ref
                .as_ref()
                .ok_or_else(|| "interactive browser node is missing its ref".to_owned())?;
            let selector = node
                .selector
                .as_ref()
                .ok_or_else(|| "interactive browser node is missing its selector".to_owned())?;
            let fingerprint = BrowserTargetFingerprint {
                tag: node.tag.clone(),
                role: node.role.clone(),
                name: node.name.clone(),
                input_type: node.input_type.clone(),
                href: node.href.clone(),
                sensitive: node.sensitive,
            };
            targets.insert(
                target_ref.clone(),
                BrowserTargetRecord {
                    frame_path: node.frame_path.clone(),
                    selector: selector.clone(),
                    marker: marker.clone(),
                    marker_value: target_ref.clone(),
                    fingerprint,
                    sensitive: node.sensitive,
                    consequential: node.consequential,
                },
            );
        }
        nodes.push(BrowserSemanticNode {
            kind: node.kind,
            target_ref: node.target_ref,
            tag: node.tag,
            role: node.role,
            name: node.name,
            frame: node.frame,
            text: node.text,
            value: node.value,
            href: node.href,
            input_type: node.input_type,
            disabled: node.disabled,
            checked: node.checked,
            sensitive: node.sensitive,
            actions: node.actions,
            bounds: node.bounds,
        });
    }

    // One atomic completion: recheck capability, workspace, visibility,
    // halt, controller, grant, instance, epoch, and load state before
    // storing the semantic snapshot. No window for Stop/revoke to
    // repopulate after a separate record call.
    registry.complete_semantic_snapshot(
        capability_id,
        &browser_id,
        instance_id,
        document_epoch,
        snapshot_id.clone(),
        targets,
    )?;

    Ok(BrowserPageSnapshot {
        browser_id,
        snapshot_id,
        document_epoch,
        content_trust: BrowserContentTrust::UntrustedPage,
        url: raw.url,
        title: raw.title,
        viewport: raw.viewport,
        nodes,
        frames: raw.frames,
        truncated: raw.truncated,
    })
}

async fn native_consequential_action_choice(
    app: &AppHandle,
    origin: &BrowserOrigin,
    action_type: &str,
    target_label: Option<&str>,
) -> Result<bool, String> {
    let origin = crate::native_security_label(origin.as_str());
    let action = crate::native_security_label(action_type);
    let target = target_label
        .map(crate::native_security_label)
        .unwrap_or_else(|| "an unlabeled target".to_owned());
    let (sender, receiver) = oneshot::channel();
    let mut dialog = app
        .dialog()
        .message(format!(
            "Allow the agent to {action} on {origin}?\n\nTarget: {target}\n\nThe target label came from the page and is untrusted. Confirm only if this is the external effect you expect."
        ))
        .title("Confirm browser action")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Allow action".to_owned(),
            "Cancel".to_owned(),
        ));
    if let Some(window) = app.get_window("main") {
        dialog = dialog.parent(&window);
    }
    dialog.show(move |approved| {
        let _ = sender.send(approved);
    });
    receiver
        .await
        .map_err(|_| "the native browser confirmation closed unexpectedly".to_owned())
}

pub(crate) async fn native_browser_upload_choice(
    app: &AppHandle,
    origin: &BrowserOrigin,
    target_label: &str,
    file: &BrowserUploadFile,
) -> Result<bool, String> {
    let origin = crate::native_security_label(origin.as_str());
    let target = crate::native_security_label(target_label);
    let filename = crate::native_security_label(&file.filename);
    let size = tidebreak_core::format_bytes(file.binding.byte_len);
    let (sender, receiver) = oneshot::channel();
    let mut dialog = app
        .dialog()
        .message(format!(
            "Allow the agent to upload {filename} ({size}) to {origin}?\n\nTarget: {target}\n\nThe site controls the target and may react when the file is attached. Tidebreak will attach only these exact confirmed bytes. Every upload requires a separate confirmation."
        ))
        .title("Confirm browser upload")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Upload file".to_owned(),
            "Cancel".to_owned(),
        ));
    if let Some(window) = app.get_window("main") {
        dialog = dialog.parent(&window);
    }
    dialog.show(move |approved| {
        let _ = sender.send(approved);
    });
    receiver
        .await
        .map_err(|_| "the native browser upload confirmation closed unexpectedly".to_owned())
}

pub(crate) async fn execute_browser_upload(
    app: AppHandle,
    registry: BrowserRegistry,
    capability_id: Uuid,
    workspace_id: String,
    arguments: tidebreak_core::BrowserUploadArgs,
    file: BrowserUploadFile,
) -> Result<tidebreak_core::BrowserUploadResult, String> {
    let target = match registry.semantic_target(
        &arguments.browser_id,
        &workspace_id,
        &arguments.snapshot_id,
        arguments.document_epoch,
        &arguments.target_ref,
    ) {
        Ok(target) => target,
        Err(error) => {
            return Ok(browser_upload_result(
                &arguments,
                browser_upload_status_from_target_error(error),
                target_error_message(error),
                None,
            ));
        }
    };
    if !is_file_input(&target) {
        return Ok(browser_upload_result(
            &arguments,
            tidebreak_core::BrowserUploadStatus::InvalidTarget,
            "The selected target is not a file input. Use a file-input ref from the latest snapshot.",
            None,
        ));
    }

    let host = registry.begin_agent_observation(capability_id, &arguments.browser_id)?;
    let origin = host
        .url
        .as_deref()
        .and_then(BrowserOrigin::from_url)
        .ok_or_else(|| "browser has no authorized HTTP origin".to_owned())?;
    let fence = registry.observation_fence(capability_id, &arguments.browser_id)?;
    let label = browser_label(&arguments.browser_id)?;
    let webview = app
        .get_webview(&label)
        .ok_or_else(|| "browser session is not open".to_owned())?;
    let script = browser_upload_script(&target, &file)?;
    let authorization = NativeUploadAuthorization {
        registry: registry.clone(),
        capability_id,
        workspace_id: workspace_id.clone(),
        origin,
        fence,
        arguments: arguments.clone(),
        target,
    };
    let raw = evaluate_browser_upload(&webview, script, authorization).await?;
    if raw.status == tidebreak_core::BrowserUploadStatus::Uploaded {
        registry.invalidate_semantic_snapshot(
            &arguments.browser_id,
            &workspace_id,
            &arguments.snapshot_id,
        );
        return Ok(browser_upload_result(
            &arguments,
            raw.status,
            "File attached. Take a new snapshot before the next browser action.",
            Some((&file.filename, file.binding.byte_len)),
        ));
    }
    Ok(browser_upload_result(
        &arguments,
        raw.status,
        &raw.message,
        None,
    ))
}

pub(crate) fn is_file_input(target: &BrowserTargetRecord) -> bool {
    target.fingerprint.tag == "input"
        && target.fingerprint.input_type.as_deref() == Some("file")
        && target.fingerprint.sensitive
}

fn browser_upload_status_from_target_error(
    error: BrowserTargetError,
) -> tidebreak_core::BrowserUploadStatus {
    match error {
        BrowserTargetError::StaleTarget => tidebreak_core::BrowserUploadStatus::StaleTarget,
        BrowserTargetError::BrowserHidden => tidebreak_core::BrowserUploadStatus::HiddenTab,
    }
}

pub(crate) fn browser_upload_result(
    request: &tidebreak_core::BrowserUploadArgs,
    status: tidebreak_core::BrowserUploadStatus,
    message: &str,
    file: Option<(&str, u64)>,
) -> tidebreak_core::BrowserUploadResult {
    tidebreak_core::BrowserUploadResult {
        browser_id: request.browser_id.clone(),
        snapshot_id: request.snapshot_id.clone(),
        document_epoch: request.document_epoch,
        target_ref: request.target_ref.clone(),
        status,
        message: message.to_owned(),
        requires_resnapshot: matches!(
            status,
            tidebreak_core::BrowserUploadStatus::Uploaded
                | tidebreak_core::BrowserUploadStatus::StaleTarget
        ),
        filename: file.map(|(filename, _)| filename.to_owned()),
        bytes: file.map(|(_, bytes)| bytes),
    }
}

fn marker_for_snapshot(snapshot_id: &str) -> String {
    format!("__tidebreak_ref_{}", snapshot_id.replace('-', ""))
}

// ── Deterministic waits ─────────────────────────────────────────────

pub(crate) async fn browser_wait(
    app: &AppHandle,
    registry: &BrowserRegistry,
    capability_id: Uuid,
    arguments: tidebreak_core::BrowserWaitArgs,
) -> Result<tidebreak_core::BrowserWaitResult, String> {
    if !arguments.is_well_formed() {
        return Err("browser wait request is not valid".to_owned());
    }
    // Observation chained from a prior snapshot: re-check live authorization
    // without clearing the stored snapshot (begin_agent_observation does not
    // acquire or mutate controller state), then validate the caller's
    // snapshot_id and document_epoch against the stored snapshot before
    // dispatching the poll.
    let host_snapshot = registry.begin_agent_observation(capability_id, &arguments.browser_id)?;
    registry.validate_snapshot_id(
        &arguments.browser_id,
        &host_snapshot.workspace_id,
        &arguments.snapshot_id,
        arguments.document_epoch,
    )?;
    let origin = host_snapshot
        .url
        .as_deref()
        .and_then(BrowserOrigin::from_url)
        .ok_or_else(|| "browser has no authorized HTTP origin".to_owned())?;
    let workspace_id = host_snapshot.workspace_id;
    let browser_id = arguments.browser_id.clone();
    let app = app.clone();
    let dispatch_registry = registry.clone();
    registry
        .dispatch_agent(
            capability_id,
            &browser_id,
            &origin,
            BrowserGrantCapability::BrowserObserveOrigin,
            "wait",
            None,
            BrowserDispatchEffect::Observe,
            None,
            move || async move {
                poll_wait_condition(
                    app,
                    dispatch_registry,
                    capability_id,
                    workspace_id,
                    arguments,
                )
                .await
            },
        )
        .await
}

async fn poll_wait_condition(
    app: AppHandle,
    registry: BrowserRegistry,
    capability_id: Uuid,
    workspace_id: String,
    arguments: tidebreak_core::BrowserWaitArgs,
) -> Result<tidebreak_core::BrowserWaitResult, String> {
    use tidebreak_core::{BrowserWaitCondition, BrowserWaitStatus};

    let label = browser_label(&arguments.browser_id)?;
    let webview = app
        .get_webview(&label)
        .ok_or_else(|| "browser session is not open".to_owned())?;

    let timeout_ms = arguments.bounded_timeout_ms();
    let poll_ms = (timeout_ms / 20).clamp(80, 500);
    let start = std::time::Instant::now();

    // The fence pins the controlled instance this wait started against; the
    // halt receiver lets Stop abort an in-flight probe or sleep immediately.
    let start_fence = registry.observation_fence(capability_id, &arguments.browser_id)?;
    registry.validate_snapshot_id(
        &arguments.browser_id,
        &workspace_id,
        &arguments.snapshot_id,
        arguments.document_epoch,
    )?;
    let mut halt = registry.subscribe_halt(&arguments.browser_id, &workspace_id)?;

    // Capture the starting URL only after the caller's exact snapshot and
    // document epoch have been revalidated inside the serialized dispatch.
    let start_snapshot = registry.snapshot(&arguments.browser_id, &workspace_id)?;
    if start_snapshot.document_epoch != Some(arguments.document_epoch) {
        return Err("browser document changed since the snapshot was taken".to_owned());
    }
    let start_url = start_snapshot.url.clone();

    loop {
        let fence = registry.observation_fence(capability_id, &arguments.browser_id)?;
        if fence.instance_id != start_fence.instance_id {
            return Err("browser session was replaced while waiting".to_owned());
        }
        if fence.document_epoch != arguments.document_epoch
            && !matches!(arguments.condition, BrowserWaitCondition::UrlChanged)
        {
            return Err("browser document changed since the snapshot was taken".to_owned());
        }
        let snapshot = registry.snapshot(&arguments.browser_id, &workspace_id)?;
        let Some(document_epoch) = snapshot.document_epoch else {
            return Ok(wait_result(
                &arguments.browser_id,
                &snapshot,
                BrowserWaitStatus::TimedOut,
                "browser document epoch is unavailable".to_owned(),
            ));
        };
        if document_epoch != arguments.document_epoch
            && !matches!(arguments.condition, BrowserWaitCondition::UrlChanged)
        {
            return Err("browser document changed since the snapshot was taken".to_owned());
        }

        if *halt.borrow_and_update()
            || snapshot
                .agent_access
                .as_ref()
                .is_some_and(|access| access.halted)
        {
            return Ok(wait_result(
                &arguments.browser_id,
                &snapshot,
                BrowserWaitStatus::Stopped,
                "Browser control was stopped by the user.".to_owned(),
            ));
        }

        // Race the condition probe against the halt latch so Stop aborts an
        // in-flight text probe instead of letting it settle.
        let satisfied = tokio::select! {
            changed = halt.changed() => {
                if changed.is_err() {
                    return Err("browser session was replaced while waiting".to_owned());
                }
                continue;
            }
            satisfied = async {
                // Race each text probe against the single remaining deadline
                // budget so a slow JavaScript evaluation cannot overshoot
                // the advertised hard timeout by up to 10 s.
                let remaining = timeout_ms.saturating_sub(start.elapsed().as_millis() as u64);
                tokio::time::timeout(
                    std::time::Duration::from_millis(remaining),
                    evaluate_wait_condition(
                        &webview,
                        &arguments.condition,
                        &snapshot,
                        start_url.as_deref(),
                    ),
                )
                .await
                .unwrap_or_default()
            } => satisfied,
        };

        if satisfied {
            // Completion-time fencing: report Resolved only while the
            // capability, grant, controller, visibility, and instance that
            // authorized this wait are all still live.
            let final_snapshot = registry.snapshot(&arguments.browser_id, &workspace_id)?;
            if *halt.borrow_and_update()
                || final_snapshot
                    .agent_access
                    .as_ref()
                    .is_some_and(|access| access.halted)
            {
                return Ok(wait_result(
                    &arguments.browser_id,
                    &final_snapshot,
                    BrowserWaitStatus::Stopped,
                    "Browser control was stopped by the user.".to_owned(),
                ));
            }
            let fence = registry.observation_fence(capability_id, &arguments.browser_id)?;
            if fence.instance_id != start_fence.instance_id {
                return Err("browser session was replaced while waiting".to_owned());
            }
            // UrlChanged resolves across documents by design. Every other
            // condition remains pinned to the caller's document epoch.
            if matches!(arguments.condition, BrowserWaitCondition::UrlChanged)
                || (document_epoch == arguments.document_epoch
                    && fence.document_epoch == arguments.document_epoch)
            {
                return Ok(wait_result(
                    &arguments.browser_id,
                    &final_snapshot,
                    BrowserWaitStatus::Resolved,
                    "Wait condition satisfied.".to_owned(),
                ));
            }
            return Err("browser document changed since the snapshot was taken".to_owned());
        }

        let elapsed = start.elapsed().as_millis() as u64;
        if elapsed >= timeout_ms {
            let final_snapshot = registry.snapshot(&arguments.browser_id, &workspace_id)?;
            return Ok(wait_result(
                &arguments.browser_id,
                &final_snapshot,
                BrowserWaitStatus::TimedOut,
                format!("Wait timed out after {timeout_ms} ms."),
            ));
        }

        // Bound the sleep to the remaining deadline, and let Stop interrupt
        // it instead of waiting out the full poll interval.
        let sleep_ms = poll_ms.min(timeout_ms - elapsed);
        tokio::select! {
            changed = halt.changed() => {
                if changed.is_err() {
                    return Err("browser session was replaced while waiting".to_owned());
                }
            }
            () = tokio::time::sleep(std::time::Duration::from_millis(sleep_ms)) => {}
        }
    }
}

/// Evaluate one wait condition. Text probes are racy against the page
/// and may take up to 10 s (JavaScript timeout), so the caller must
/// race them against the *remaining* deadline budget and the halt
/// latch; they are not invoked directly.
async fn evaluate_wait_condition(
    webview: &Webview,
    condition: &tidebreak_core::BrowserWaitCondition,
    snapshot: &BrowserSnapshot,
    start_url: Option<&str>,
) -> bool {
    use tidebreak_core::BrowserWaitCondition;

    if let Some(satisfied) = registry_wait_condition(condition, snapshot, start_url) {
        return satisfied;
    }

    match condition {
        BrowserWaitCondition::TextPresent { text } => {
            page_contains_text(webview, text).await.unwrap_or(false)
        }
        BrowserWaitCondition::TextAbsent { text } => {
            !page_contains_text(webview, text).await.unwrap_or(true)
        }
        BrowserWaitCondition::LoadState { .. } | BrowserWaitCondition::UrlChanged => {
            unreachable!("registry-only waits returned above")
        }
    }
}

fn registry_wait_condition(
    condition: &tidebreak_core::BrowserWaitCondition,
    snapshot: &BrowserSnapshot,
    start_url: Option<&str>,
) -> Option<bool> {
    use tidebreak_core::BrowserWaitCondition;

    match condition {
        BrowserWaitCondition::LoadState { state } => Some(snapshot.load_state == Some(*state)),
        BrowserWaitCondition::UrlChanged => Some(snapshot.url.as_deref() != start_url),
        BrowserWaitCondition::TextPresent { .. } | BrowserWaitCondition::TextAbsent { .. } => None,
    }
}

fn wait_result(
    browser_id: &str,
    snapshot: &BrowserSnapshot,
    status: tidebreak_core::BrowserWaitStatus,
    message: String,
) -> tidebreak_core::BrowserWaitResult {
    tidebreak_core::BrowserWaitResult {
        browser_id: browser_id.to_owned(),
        status,
        message,
        document_epoch: snapshot.document_epoch.unwrap_or(0),
        url: snapshot.url.clone(),
        title: snapshot.title.clone(),
    }
}

async fn page_contains_text(webview: &Webview, text: &str) -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        let raw: RawTextProbe = evaluate_json(webview, &wait_text_script(text)?).await?;
        if raw.uninspectable_regions > 0 {
            return Err(
                "browser text wait requires human takeover because visible page content cannot be inspected safely"
                    .to_owned(),
            );
        }
        Ok(raw.contains)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (webview, text);
        Err("semantic browser control is not available on this platform yet".to_owned())
    }
}

// ── Epoch-bound screenshots ───────────────────────────────────────

pub(crate) async fn browser_screenshot(
    app: &AppHandle,
    registry: &BrowserRegistry,
    capability_id: Uuid,
    arguments: tidebreak_core::BrowserScreenshotArgs,
) -> Result<tidebreak_core::BrowserScreenshotResult, String> {
    if !arguments.is_well_formed() {
        return Err("browser screenshot request is not valid".to_owned());
    }
    // Observation chained from a prior snapshot: use begin_agent_observation
    // so the stored snapshot survives for validate_snapshot_id below.
    let host_snapshot = registry.begin_agent_observation(capability_id, &arguments.browser_id)?;
    if !host_snapshot
        .engine
        .as_ref()
        .is_some_and(|engine| engine.capabilities.screenshot)
    {
        return Err(
            "browser screenshot requires human takeover because this engine cannot prove closed-shadow privacy"
                .to_owned(),
        );
    }
    let origin = host_snapshot
        .url
        .as_deref()
        .and_then(BrowserOrigin::from_url)
        .ok_or_else(|| "browser has no authorized HTTP origin".to_owned())?;
    let workspace_id = host_snapshot.workspace_id;
    let browser_id = arguments.browser_id.clone();
    let app = app.clone();
    let dispatch_registry = registry.clone();
    registry
        .dispatch_agent(
            capability_id,
            &browser_id,
            &origin,
            BrowserGrantCapability::BrowserObserveOrigin,
            "screenshot",
            None,
            BrowserDispatchEffect::Observe,
            None,
            move || async move {
                capture_screenshot(
                    app,
                    dispatch_registry,
                    capability_id,
                    workspace_id,
                    arguments,
                )
                .await
            },
        )
        .await
}

async fn capture_screenshot(
    app: AppHandle,
    registry: BrowserRegistry,
    capability_id: Uuid,
    workspace_id: String,
    arguments: tidebreak_core::BrowserScreenshotArgs,
) -> Result<tidebreak_core::BrowserScreenshotResult, String> {
    use tidebreak_core::BrowserScreenshotResult;

    let label = browser_label(&arguments.browser_id)?;

    // Capture the instance identity and fence before the async screenshot
    // so a Stop/replace cannot pass a stale image into the live registry.
    let start_fence = registry
        .observation_fence(capability_id, &arguments.browser_id)
        .map_err(|error| format!("screenshot authorization lapsed: {error}"))?;
    let instance_id = start_fence.instance_id;
    let document_epoch = start_fence.document_epoch;

    if document_epoch != arguments.document_epoch {
        return Err("browser document changed since the snapshot was taken".to_owned());
    }

    // Validate the snapshot id inside the same lock acquisition (before
    // the async image capture) so a replaced snapshot record is caught
    // before we spend time on the expensive native screenshot.
    registry.validate_snapshot_id(
        &arguments.browser_id,
        &workspace_id,
        &arguments.snapshot_id,
        document_epoch,
    )?;

    let webview = app
        .get_webview(&label)
        .ok_or_else(|| "browser session is not open".to_owned())?;

    let privacy_watch = format!("__tidebreak_screenshot_privacy_{}", Uuid::new_v4().simple());
    let initial_privacy: RawScreenshotPrivacyScan =
        evaluate_json(&webview, &screenshot_privacy_script(&privacy_watch, false)?).await?;
    if initial_privacy.sensitive_fields > 0 || initial_privacy.uninspectable_regions > 0 {
        let _ = evaluate_json::<RawScreenshotPrivacyScan>(
            &webview,
            &screenshot_privacy_script(&privacy_watch, true)?,
        )
        .await;
        return Err(screenshot_human_takeover_message(&initial_privacy));
    }

    let image_result =
        capture_browser_image(&webview, arguments.max_width, arguments.max_height).await;
    let final_privacy: RawScreenshotPrivacyScan = evaluate_json(
        &webview,
        &screenshot_privacy_script(&privacy_watch, true)?,
    )
    .await
    .map_err(|error| {
        format!(
            "browser screenshot requires human takeover because the privacy scan could not be completed: {error}"
        )
    })?;
    if final_privacy.sensitive_fields > 0
        || final_privacy.uninspectable_regions > 0
        || final_privacy.changed
    {
        return Err(screenshot_human_takeover_message(&final_privacy));
    }
    let image_base64 = image_result?;

    // One atomic completion: recheck capability, workspace, visibility,
    // halt, controller, grant, instance, document epoch, and stored
    // snapshot identity before recording the screenshot. This closes
    // the three-lock TOCTOU gap that existed between observation_fence,
    // validate_snapshot_id, and record_screenshot_epoch.
    registry
        .complete_screenshot_recording(
            capability_id,
            &arguments.browser_id,
            instance_id,
            document_epoch,
            &arguments.snapshot_id,
        )
        .map_err(|error| format!("screenshot recording failed: {error}"))?;

    Ok(BrowserScreenshotResult {
        browser_id: arguments.browser_id,
        snapshot_id: arguments.snapshot_id,
        document_epoch,
        image_base64,
        mime_type: "image/png".to_owned(),
    })
}

fn screenshot_human_takeover_message(scan: &RawScreenshotPrivacyScan) -> String {
    if scan.uninspectable_regions > 0 {
        return "browser screenshot requires human takeover because cross-origin or closed-shadow content cannot be checked for sensitive fields".to_owned();
    }
    if scan.sensitive_fields > 0 {
        return "browser screenshot requires human takeover because the page contains a password, file, or verification-code field".to_owned();
    }
    "browser screenshot requires human takeover because the page changed during privacy-safe capture"
        .to_owned()
}

#[cfg(target_os = "macos")]
async fn capture_browser_image(
    webview: &Webview,
    max_width: Option<u64>,
    max_height: Option<u64>,
) -> Result<String, String> {
    use std::ffi::c_void;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::encode::{Encode, Encoding};
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSError;
    use objc2_web_kit::WKWebView;
    use tidebreak_core::browser::{
        MAX_BROWSER_SCREENSHOT_DIMENSION, MAX_BROWSER_SCREENSHOT_PNG_BYTES,
    };
    use tokio::{sync::oneshot, time::timeout};

    const SCREENSHOT_TIMEOUT_SECONDS: u64 = 30;

    /// Core Graphics geometry mirrored with the exact Objective-C encodings
    /// so `msg_send!` can pass and return `NSRect` by value on every macOS
    /// architecture. The layouts match `objc2-core-foundation`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }

    unsafe impl Encode for CGPoint {
        const ENCODING: Encoding =
            Encoding::Struct("CGPoint", &[Encoding::Double, Encoding::Double]);
    }

    unsafe impl Encode for CGSize {
        const ENCODING: Encoding =
            Encoding::Struct("CGSize", &[Encoding::Double, Encoding::Double]);
    }

    unsafe impl Encode for CGRect {
        const ENCODING: Encoding =
            Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
    }

    // Convert an NSImage snapshot to PNG base64 via NSBitmapImageRep.
    unsafe fn snapshot_to_png_base64(
        snapshot: *mut AnyObject,
        error: *mut NSError,
    ) -> Result<String, String> {
        if !error.is_null() {
            let msg = (&*error).localizedDescription().to_string();
            return Err(format!("screenshot failed: {msg}"));
        }
        if snapshot.is_null() {
            return Err("screenshot produced no image".to_owned());
        }

        // [snapshot representations] → NSArray<NSImageRep>
        let representations: *mut AnyObject = msg_send![snapshot, representations];
        if representations.is_null() {
            return Err("screenshot has no image representations".to_owned());
        }
        let representation_count: usize = msg_send![representations, count];
        if representation_count == 0 {
            return Err("screenshot image has zero representations".to_owned());
        }

        let bitmap_class = AnyClass::get(c"NSBitmapImageRep")
            .ok_or_else(|| "AppKit NSBitmapImageRep is unavailable".to_owned())?;
        let dictionary_class = AnyClass::get(c"NSDictionary")
            .ok_or_else(|| "Foundation NSDictionary is unavailable".to_owned())?;

        // AppKit declares the PNG properties parameter nonnull, so pass an
        // empty dictionary rather than nil.
        let empty_properties: *mut AnyObject = msg_send![dictionary_class, dictionary];
        if empty_properties.is_null() {
            return Err("screenshot PNG properties were unavailable".to_owned());
        }

        // NSBitmapImageFileTypePNG = 4
        let png_type: usize = 4;
        let png_data: *mut AnyObject = msg_send![
            bitmap_class,
            representationOfImageRepsInArray: representations,
            usingType: png_type,
            properties: empty_properties
        ];
        if png_data.is_null() {
            return Err("screenshot PNG conversion failed".to_owned());
        }

        let bytes_ptr: *const c_void = msg_send![png_data, bytes];
        let byte_len: usize = msg_send![png_data, length];
        if bytes_ptr.is_null() || byte_len == 0 {
            return Err("screenshot PNG data is empty".to_owned());
        }
        if byte_len > MAX_BROWSER_SCREENSHOT_PNG_BYTES {
            return Err(format!(
                "screenshot PNG of {byte_len} bytes exceeds the encoded-image ceiling"
            ));
        }
        let buf = std::slice::from_raw_parts(bytes_ptr.cast::<u8>(), byte_len);
        use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
        Ok(BASE64.encode(buf))
    }

    /// Build a `WKSnapshotConfiguration` cropping the capture when the view
    /// exceeds the requested or absolute maximum dimensions. Returns `None`
    /// when the full visible viewport already fits.
    unsafe fn snapshot_configuration(
        view: &WKWebView,
        max_width: Option<u64>,
        max_height: Option<u64>,
    ) -> Result<Option<Retained<AnyObject>>, String> {
        let bounds: CGRect = msg_send![view, bounds];
        let ceiling = MAX_BROWSER_SCREENSHOT_DIMENSION as f64;
        let mut width = bounds.size.width.min(ceiling);
        let mut height = bounds.size.height.min(ceiling);
        if let Some(limit) = max_width {
            width = width.min(limit as f64);
        }
        // A `max_height` of zero means "use the viewport height".
        if let Some(limit) = max_height.filter(|limit| *limit > 0) {
            height = height.min(limit as f64);
        }
        if width >= bounds.size.width && height >= bounds.size.height {
            return Ok(None);
        }
        let configuration_class = AnyClass::get(c"WKSnapshotConfiguration")
            .ok_or_else(|| "WebKit WKSnapshotConfiguration is unavailable".to_owned())?;
        let configuration: Option<Retained<AnyObject>> = msg_send![configuration_class, new];
        let configuration =
            configuration.ok_or_else(|| "screenshot configuration was unavailable".to_owned())?;
        let rect = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize { width, height },
        };
        let _: () = msg_send![&*configuration, setRect: rect];
        Ok(Some(configuration))
    }

    let (sender, receiver) = oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(sender)));
    let block_sender = Arc::clone(&sender);

    with_browser_webview(webview, move |view| unsafe {
        let configuration = match snapshot_configuration(view, max_width, max_height) {
            Ok(configuration) => configuration,
            Err(error) => {
                if let Some(sender) = sender.lock().ok().and_then(|mut s| s.take()) {
                    let _ = sender.send(Err(error));
                }
                return;
            }
        };
        let configuration_ptr: *mut AnyObject = match &configuration {
            Some(configuration) => Retained::as_ptr(configuration).cast_mut(),
            None => std::ptr::null_mut(),
        };
        let handler = RcBlock::new(move |snapshot: *mut AnyObject, error: *mut NSError| {
            let Some(sender) = block_sender.lock().ok().and_then(|mut s| s.take()) else {
                return;
            };
            let result = snapshot_to_png_base64(snapshot, error);
            let _ = sender.send(result);
        });
        let _: () = msg_send![
            view,
            takeSnapshotWithConfiguration: configuration_ptr,
            completionHandler: &*handler
        ];
    })?;

    timeout(Duration::from_secs(SCREENSHOT_TIMEOUT_SECONDS), receiver)
        .await
        .map_err(|_| "screenshot capture timed out".to_owned())?
        .map_err(|_| "screenshot capture was interrupted".to_owned())?
}

#[cfg(not(target_os = "macos"))]
async fn capture_browser_image(
    _webview: &Webview,
    _max_width: Option<u64>,
    _max_height: Option<u64>,
) -> Result<String, String> {
    Err("screenshot capture is not available on this platform yet".to_owned())
}

// ── Native semantic act ───────────────────────────────────────────

pub(crate) async fn browser_native_act(
    app: &AppHandle,
    registry: &BrowserRegistry,
    capability_id: Uuid,
    arguments: tidebreak_core::BrowserActArgs,
) -> Result<tidebreak_core::BrowserActResult, String> {
    use tidebreak_core::BrowserActStatus;

    if !arguments.is_well_formed() {
        return Err("browser action request is not valid".to_owned());
    }
    if let Some(value) = arguments.action.value() {
        if value.chars().count() > MAX_ACTION_VALUE_CHARS {
            return Err("browser action value is too long".to_owned());
        }
    }

    let host_snapshot = registry.begin_agent_observation(capability_id, &arguments.browser_id)?;
    if !host_snapshot
        .engine
        .as_ref()
        .is_some_and(|engine| engine.capabilities.semantic_actions)
    {
        return Ok(act_result(
            &arguments,
            BrowserActStatus::UnsupportedNative,
            "Trusted native browser input is not available yet.",
        ));
    }

    let fence = registry.observation_fence(capability_id, &arguments.browser_id)?;
    let workspace_id = host_snapshot.workspace_id;
    let origin = host_snapshot
        .url
        .as_deref()
        .and_then(BrowserOrigin::from_url)
        .ok_or_else(|| "browser has no authorized HTTP origin".to_owned())?;
    let target = match registry.semantic_target(
        &arguments.browser_id,
        &workspace_id,
        &arguments.snapshot_id,
        arguments.document_epoch,
        &arguments.target_ref,
    ) {
        Ok(target) => target,
        Err(error) => {
            return Ok(act_result(
                &arguments,
                act_status_from_target_error(error),
                target_error_message(error),
            ));
        }
    };
    if target.sensitive {
        let _ = registry.set_agent_action(
            capability_id,
            &arguments.browser_id,
            Some("Waiting for human input"),
            true,
        );
        return Ok(act_result(
            &arguments,
            BrowserActStatus::HumanTakeoverRequired,
            "Password, file, and verification-code fields require human takeover.",
        ));
    }

    let action_type = arguments.action.kind().to_owned();
    let target_label =
        (!target.fingerprint.name.is_empty()).then(|| target.fingerprint.name.clone());
    let consequential =
        native_action_requires_confirmation(&origin, target.consequential, &arguments.action);
    let confirmation_id = if consequential {
        let _ = registry.set_agent_action(
            capability_id,
            &arguments.browser_id,
            Some("Waiting for native confirmation"),
            false,
        );
        if !native_consequential_action_choice(app, &origin, &action_type, target_label.as_deref())
            .await?
        {
            let _ = registry.set_agent_action(capability_id, &arguments.browser_id, None, false);
            return Ok(act_result(
                &arguments,
                BrowserActStatus::HumanTakeoverRequired,
                "The user declined this consequential browser action. Do not retry it without direction.",
            ));
        }
        Some(registry.record_native_confirmation(
            capability_id,
            &arguments.browser_id,
            &origin,
            BrowserGrantCapability::BrowserControlOrigin,
            &action_type,
            target_label.as_deref(),
            None,
        )?)
    } else {
        None
    };
    let effect = if consequential {
        BrowserDispatchEffect::Consequential
    } else {
        BrowserDispatchEffect::Mutate
    };
    let app = app.clone();
    let dispatch_registry = registry.clone();
    let browser_id = arguments.browser_id.clone();
    let dispatch_origin = origin.clone();
    registry
        .dispatch_agent(
            capability_id,
            &browser_id,
            &origin,
            BrowserGrantCapability::BrowserControlOrigin,
            &action_type,
            target_label.as_deref(),
            effect,
            confirmation_id,
            move || async move {
                execute_native_action(
                    app,
                    dispatch_registry,
                    capability_id,
                    workspace_id,
                    dispatch_origin,
                    fence,
                    arguments,
                )
                .await
            },
        )
        .await
}

fn native_action_requires_confirmation(
    origin: &BrowserOrigin,
    target_consequential: bool,
    action: &tidebreak_core::BrowserAction,
) -> bool {
    use tidebreak_core::BrowserAction;

    if origin.is_loopback() {
        return false;
    }
    match action {
        BrowserAction::Select { .. } => true,
        BrowserAction::Click | BrowserAction::Check { .. } | BrowserAction::Press { .. } => {
            target_consequential
        }
        BrowserAction::Focus
        | BrowserAction::Hover
        | BrowserAction::Fill { .. }
        | BrowserAction::ScrollIntoView => false,
    }
}

async fn execute_native_action(
    app: AppHandle,
    registry: BrowserRegistry,
    capability_id: Uuid,
    workspace_id: String,
    origin: BrowserOrigin,
    fence: BrowserObservationFence,
    arguments: tidebreak_core::BrowserActArgs,
) -> Result<tidebreak_core::BrowserActResult, String> {
    use tidebreak_core::BrowserActStatus;

    let target = match registry.semantic_target(
        &arguments.browser_id,
        &workspace_id,
        &arguments.snapshot_id,
        arguments.document_epoch,
        &arguments.target_ref,
    ) {
        Ok(target) => target,
        Err(error) => {
            return Ok(act_result(
                &arguments,
                act_status_from_target_error(error),
                target_error_message(error),
            ));
        }
    };
    if target.sensitive {
        return Ok(act_result(
            &arguments,
            BrowserActStatus::HumanTakeoverRequired,
            "Password, file, and verification-code fields require human takeover.",
        ));
    }

    let label = browser_label(&arguments.browser_id)?;
    let webview = app
        .get_webview(&label)
        .ok_or_else(|| "browser session is not open".to_owned())?;
    let resolution_script = native_action_resolution_script(&target, &arguments.action)?;
    let (resolution, dispatch_error) = match resolve_and_dispatch_native_action(
        &webview,
        registry.clone(),
        capability_id,
        workspace_id.clone(),
        origin.clone(),
        fence,
        &arguments,
        resolution_script,
        NativeActionDispatchPhase::Initial,
        None,
    )
    .await
    {
        Ok(NativeActionDispatchOutcome::Resolved {
            resolution,
            dispatch_error,
        }) => (resolution, dispatch_error),
        Ok(NativeActionDispatchOutcome::TargetRejected(error)) => {
            return Ok(act_result(
                &arguments,
                act_status_from_target_error(error),
                target_error_message(error),
            ));
        }
        Err(failure) => return Ok(native_failure_result(arguments, failure, None, false)),
    };

    let resolution = *resolution;
    let status = native_resolution_status(resolution.status);
    let performed = resolution.status == NativeActionResolutionStatus::Ready;
    if let Some(failure) = dispatch_error {
        let requires_resnapshot = failure.requires_resnapshot();
        if requires_resnapshot {
            registry.invalidate_semantic_snapshot(
                &arguments.browser_id,
                &workspace_id,
                &arguments.snapshot_id,
            );
        }
        return Ok(native_failure_result(
            arguments,
            failure,
            Some(&resolution),
            requires_resnapshot,
        ));
    }
    if performed {
        registry.invalidate_semantic_snapshot(
            &arguments.browser_id,
            &workspace_id,
            &arguments.snapshot_id,
        );
        if let Err(failure) = wait_for_native_action_processing(
            &webview,
            &arguments.action,
            NativeActionDispatchPhase::Initial,
        )
        .await
        {
            return Ok(native_failure_result(
                arguments,
                failure,
                Some(&resolution),
                true,
            ));
        }
        if let Err(failure) = finish_native_action(
            &webview,
            &registry,
            capability_id,
            &workspace_id,
            &origin,
            fence,
            &arguments,
            &resolution,
            &target,
        )
        .await
        {
            return Ok(native_failure_result(
                arguments,
                failure,
                Some(&resolution),
                true,
            ));
        }
    }

    Ok(tidebreak_core::BrowserActResult {
        browser_id: arguments.browser_id,
        snapshot_id: arguments.snapshot_id,
        document_epoch: arguments.document_epoch,
        target_ref: arguments.target_ref,
        action: arguments.action.kind().to_owned(),
        status,
        message: if performed {
            "Action completed. Take a new snapshot before the next action.".to_owned()
        } else {
            resolution.message
        },
        requires_resnapshot: performed || status == BrowserActStatus::StaleTarget,
        url: Some(resolution.url),
        title: Some(resolution.title),
    })
}

fn native_resolution_status(
    status: NativeActionResolutionStatus,
) -> tidebreak_core::BrowserActStatus {
    use tidebreak_core::BrowserActStatus;

    match status {
        NativeActionResolutionStatus::Ready | NativeActionResolutionStatus::NoOp => {
            BrowserActStatus::Ok
        }
        NativeActionResolutionStatus::StaleTarget => BrowserActStatus::StaleTarget,
        NativeActionResolutionStatus::HumanTakeoverRequired => {
            BrowserActStatus::HumanTakeoverRequired
        }
        NativeActionResolutionStatus::UnsupportedFrame => BrowserActStatus::UnsupportedFrame,
        NativeActionResolutionStatus::UnsupportedNative
        | NativeActionResolutionStatus::PendingNativeInput => BrowserActStatus::UnsupportedNative,
        NativeActionResolutionStatus::InvalidValue => BrowserActStatus::InvalidValue,
        NativeActionResolutionStatus::TargetObscured => BrowserActStatus::TargetObscured,
    }
}

fn target_error_message(error: BrowserTargetError) -> &'static str {
    match error {
        BrowserTargetError::StaleTarget => {
            "The page or target changed. Take a new snapshot before acting."
        }
        BrowserTargetError::BrowserHidden => {
            "Bring this browser tab to the foreground before acting."
        }
    }
}

#[derive(Debug)]
enum NativeInputFailure {
    Engine(String),
    HiddenTab(String),
    Typed {
        status: tidebreak_core::BrowserActStatus,
        message: String,
    },
    Timeout(String),
}

impl NativeInputFailure {
    fn status(&self) -> tidebreak_core::BrowserActStatus {
        match self {
            Self::Engine(_) => tidebreak_core::BrowserActStatus::EngineFailure,
            Self::HiddenTab(_) => tidebreak_core::BrowserActStatus::HiddenTab,
            Self::Typed { status, .. } => *status,
            Self::Timeout(_) => tidebreak_core::BrowserActStatus::Timeout,
        }
    }

    fn requires_resnapshot(&self) -> bool {
        !matches!(self, Self::HiddenTab(_))
    }

    fn message(self) -> String {
        match self {
            Self::Engine(message) | Self::HiddenTab(message) | Self::Timeout(message) => message,
            Self::Typed { message, .. } => message,
        }
    }
}

fn native_resolution_failure(resolution: NativeActionResolution) -> NativeInputFailure {
    NativeInputFailure::Typed {
        status: native_resolution_status(resolution.status),
        message: resolution.message,
    }
}

enum NativeActionDispatchOutcome {
    Resolved {
        resolution: Box<NativeActionResolution>,
        dispatch_error: Option<NativeInputFailure>,
    },
    TargetRejected(BrowserTargetError),
}

#[derive(Clone, Copy)]
enum NativeActionDispatchPhase {
    Initial,
    FocusVerify,
    PressKey,
    SelectInitialStep,
    FillSelectAll,
    FillInsert,
    FillVerify,
    SelectFollowUp {
        previous_selected_index: i64,
        previous_distance: u64,
    },
    ScrollFollowUp {
        previous_x: f64,
        previous_y: f64,
        previous_delta_x: f64,
        previous_delta_y: f64,
    },
}

impl NativeActionDispatchPhase {
    fn validates_registered_target(self) -> bool {
        matches!(self, Self::Initial)
    }
}

fn validate_native_follow_up_progress(
    phase: NativeActionDispatchPhase,
    resolution: &NativeActionResolution,
) -> Result<(), NativeInputFailure> {
    match phase {
        NativeActionDispatchPhase::Initial => Ok(()),
        NativeActionDispatchPhase::PressKey | NativeActionDispatchPhase::SelectInitialStep
            if resolution.target_focused =>
        {
            Ok(())
        }
        NativeActionDispatchPhase::FocusVerify
        | NativeActionDispatchPhase::PressKey
        | NativeActionDispatchPhase::SelectInitialStep => Err(NativeInputFailure::Typed {
            status: tidebreak_core::BrowserActStatus::UnsupportedNative,
            message: "The target did not retain native focus. No key was sent.".to_owned(),
        }),
        NativeActionDispatchPhase::FillSelectAll | NativeActionDispatchPhase::FillInsert
            if resolution.target_focused =>
        {
            Ok(())
        }
        NativeActionDispatchPhase::FillSelectAll | NativeActionDispatchPhase::FillInsert => {
            Err(NativeInputFailure::Typed {
                status: tidebreak_core::BrowserActStatus::UnsupportedNative,
                message: "The target did not retain native focus. No text was inserted.".to_owned(),
            })
        }
        NativeActionDispatchPhase::FillVerify => Err(NativeInputFailure::Engine(
            "The browser did not retain the requested field value.".to_owned(),
        )),
        NativeActionDispatchPhase::SelectFollowUp {
            previous_selected_index,
            previous_distance,
        } => {
            if !resolution.target_focused {
                return Err(NativeInputFailure::Typed {
                    status: tidebreak_core::BrowserActStatus::UnsupportedNative,
                    message: "The select control lost native focus. No key was sent.".to_owned(),
                });
            }
            let selected_index = resolution.selected_index.ok_or_else(|| {
                NativeInputFailure::Engine("browser select has no current option index".to_owned())
            })?;
            let option_index = resolution.option_index.ok_or_else(|| {
                NativeInputFailure::Engine(
                    "browser select has no requested option index".to_owned(),
                )
            })?;
            let distance = selected_index.abs_diff(option_index);
            if selected_index == previous_selected_index || distance >= previous_distance {
                return Err(NativeInputFailure::Engine(
                    "The browser did not move the select control toward the requested option."
                        .to_owned(),
                ));
            }
            Ok(())
        }
        NativeActionDispatchPhase::ScrollFollowUp {
            previous_x,
            previous_y,
            previous_delta_x,
            previous_delta_y,
        } => {
            let x = resolution.x.unwrap_or_default();
            let y = resolution.y.unwrap_or_default();
            let delta_x = resolution.scroll_delta_x.unwrap_or_default();
            let delta_y = resolution.scroll_delta_y.unwrap_or_default();
            let changed = (x - previous_x).abs() >= 0.5
                || (y - previous_y).abs() >= 0.5
                || (delta_x - previous_delta_x).abs() >= 0.5
                || (delta_y - previous_delta_y).abs() >= 0.5;
            if !changed {
                return Err(NativeInputFailure::Typed {
                    status: tidebreak_core::BrowserActStatus::TargetObscured,
                    message:
                        "Native scrolling did not move the target toward the visible viewport."
                            .to_owned(),
                });
            }
            Ok(())
        }
    }
}

fn native_failure_result(
    arguments: tidebreak_core::BrowserActArgs,
    failure: NativeInputFailure,
    resolution: Option<&NativeActionResolution>,
    requires_resnapshot: bool,
) -> tidebreak_core::BrowserActResult {
    tidebreak_core::BrowserActResult {
        browser_id: arguments.browser_id,
        snapshot_id: arguments.snapshot_id,
        document_epoch: arguments.document_epoch,
        target_ref: arguments.target_ref,
        action: arguments.action.kind().to_owned(),
        status: failure.status(),
        message: failure.message(),
        requires_resnapshot,
        url: resolution.map(|resolution| resolution.url.clone()),
        title: resolution.map(|resolution| resolution.title.clone()),
    }
}

#[cfg(target_os = "macos")]
struct NativeInputCallbackState {
    sender: Option<oneshot::Sender<Result<NativeActionDispatchOutcome, NativeInputFailure>>>,
    cancelled: bool,
}

#[cfg(target_os = "macos")]
struct NativeInputCancellation(std::sync::Arc<std::sync::Mutex<NativeInputCallbackState>>);

#[cfg(target_os = "macos")]
impl Drop for NativeInputCancellation {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.lock() {
            state.cancelled = true;
            state.sender.take();
        }
    }
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
async fn resolve_and_dispatch_native_action(
    webview: &Webview,
    registry: BrowserRegistry,
    capability_id: Uuid,
    workspace_id: String,
    origin: BrowserOrigin,
    fence: BrowserObservationFence,
    arguments: &tidebreak_core::BrowserActArgs,
    script: String,
    phase: NativeActionDispatchPhase,
    phase_deadline: Option<tokio::time::Instant>,
) -> Result<NativeActionDispatchOutcome, NativeInputFailure> {
    use std::sync::{Arc, Mutex};

    use block2::RcBlock;
    use objc2::{runtime::AnyObject, Message};
    use objc2_foundation::{NSError, NSString};
    use tokio::sync::oneshot;

    let deadline = phase_deadline.unwrap_or_else(|| {
        tokio::time::Instant::now() + std::time::Duration::from_secs(JAVASCRIPT_TIMEOUT_SECONDS)
    });
    let (sender, mut receiver) = oneshot::channel();
    let state = Arc::new(Mutex::new(NativeInputCallbackState {
        sender: Some(sender),
        cancelled: false,
    }));
    let _cancellation = NativeInputCancellation(Arc::clone(&state));
    let callback_state = Arc::clone(&state);
    let browser_id = arguments.browser_id.clone();
    let snapshot_id = arguments.snapshot_id.clone();
    let document_epoch = arguments.document_epoch;
    let target_ref = arguments.target_ref.clone();
    let action = arguments.action.clone();
    with_browser_webview(webview, move |view| unsafe {
        let retained_view = view.retain();
        let content_world = match browser_semantics_content_world() {
            Ok(content_world) => content_world,
            Err(error) => {
                if let Ok(mut state) = callback_state.lock() {
                    if let Some(sender) = state.sender.take() {
                        let _ = sender.send(Err(NativeInputFailure::Engine(error)));
                    }
                }
                return;
            }
        };
        let handler = RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
            let Ok(mut state) = callback_state.lock() else {
                return;
            };
            if state.cancelled {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                if let Some(sender) = state.sender.take() {
                    let _ = sender.send(Err(native_input_deadline_failure(phase, phase_deadline)));
                }
                return;
            }
            let result = (|| {
                if !error.is_null() {
                    let message = (&*error).localizedDescription().to_string();
                    return Err(NativeInputFailure::Engine(format!(
                        "browser JavaScript failed: {message}"
                    )));
                }
                if value.is_null() {
                    return Err(NativeInputFailure::Engine(
                        "browser JavaScript returned no value".to_owned(),
                    ));
                }
                let value: &NSString = &*value.cast();
                let mut resolution: NativeActionResolution =
                    serde_json::from_str(&value.to_string()).map_err(|error| {
                        NativeInputFailure::Engine(format!("invalid browser response: {error}"))
                    })?;
                registry
                    .authorize_native_action_phase(
                        capability_id,
                        &browser_id,
                        &workspace_id,
                        &origin,
                        fence,
                    )
                    .map_err(NativeInputFailure::Engine)?;
                if resolution.status == NativeActionResolutionStatus::NoOp
                    && matches!(action, tidebreak_core::BrowserAction::Focus)
                {
                    verify_native_page_focus(&retained_view)?;
                }
                if resolution.status != NativeActionResolutionStatus::Ready {
                    return Ok(NativeActionDispatchOutcome::Resolved {
                        resolution: Box::new(resolution),
                        dispatch_error: None,
                    });
                }
                if phase.validates_registered_target() {
                    let target = match registry.semantic_target(
                        &browser_id,
                        &workspace_id,
                        &snapshot_id,
                        document_epoch,
                        &target_ref,
                    ) {
                        Ok(target) => target,
                        Err(error) => {
                            return Ok(NativeActionDispatchOutcome::TargetRejected(error));
                        }
                    };
                    if target.sensitive {
                        resolution.status = NativeActionResolutionStatus::HumanTakeoverRequired;
                        resolution.message =
                            "Password, file, and verification-code fields require human takeover."
                                .to_owned();
                        return Ok(NativeActionDispatchOutcome::Resolved {
                            resolution: Box::new(resolution),
                            dispatch_error: None,
                        });
                    }
                }
                let dispatch_error = validate_native_follow_up_progress(phase, &resolution)
                    .and_then(|()| {
                        if tokio::time::Instant::now() >= deadline {
                            return Err(native_input_deadline_failure(phase, phase_deadline));
                        }
                        dispatch_native_action(&retained_view, &resolution, &action, phase)
                    })
                    .err();
                Ok(NativeActionDispatchOutcome::Resolved {
                    resolution: Box::new(resolution),
                    dispatch_error,
                })
            })();
            if let Some(sender) = state.sender.take() {
                let _ = sender.send(result);
            }
        });
        let script = NSString::from_str(&script);
        view.evaluateJavaScript_inFrame_inContentWorld_completionHandler(
            &script,
            None,
            &content_world,
            Some(&handler),
        );
    })
    .map_err(NativeInputFailure::Engine)?;

    let timeout = tokio::time::sleep_until(deadline);
    tokio::pin!(timeout);
    tokio::select! {
        result = &mut receiver => {
            result.map_err(|_| NativeInputFailure::Engine(
                "native browser input was interrupted".to_owned(),
            ))?
        }
        _ = &mut timeout => {
            let completed = {
                let mut state = state.lock().map_err(|_| NativeInputFailure::Engine(
                    "native browser input state was unavailable".to_owned(),
                ))?;
                if state.sender.is_none() {
                    true
                } else {
                    state.cancelled = true;
                    state.sender.take();
                    false
                }
            };
            if completed {
                receiver.await.map_err(|_| NativeInputFailure::Engine(
                    "native browser input was interrupted".to_owned(),
                ))?
            } else {
                Err(native_input_deadline_failure(phase, phase_deadline))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativePendingEventKind {
    Presentation,
    Mouse,
    Key,
    None,
}

fn native_pending_event_kind(
    action: &tidebreak_core::BrowserAction,
    phase: NativeActionDispatchPhase,
) -> NativePendingEventKind {
    match action {
        tidebreak_core::BrowserAction::Fill { .. } => match phase {
            NativeActionDispatchPhase::Initial
            | NativeActionDispatchPhase::FillSelectAll
            | NativeActionDispatchPhase::FillInsert => NativePendingEventKind::Presentation,
            _ => NativePendingEventKind::None,
        },
        tidebreak_core::BrowserAction::Click
        | tidebreak_core::BrowserAction::Check { .. }
        | tidebreak_core::BrowserAction::Hover
        | tidebreak_core::BrowserAction::ScrollIntoView => NativePendingEventKind::Mouse,
        tidebreak_core::BrowserAction::Press { .. }
        | tidebreak_core::BrowserAction::Select { .. } => NativePendingEventKind::Presentation,
        tidebreak_core::BrowserAction::Focus => match phase {
            NativeActionDispatchPhase::Initial => NativePendingEventKind::Presentation,
            _ => NativePendingEventKind::None,
        },
    }
}

#[cfg(target_os = "macos")]
unsafe fn ensure_native_event_processing_supported(
    view: &objc2_web_kit::WKWebView,
    kind: NativePendingEventKind,
) -> Result<(), NativeInputFailure> {
    use objc2::{msg_send, sel};
    let supported: bool = match kind {
        NativePendingEventKind::Presentation => {
            msg_send![view, respondsToSelector: sel!(_doAfterNextPresentationUpdate:)]
        }
        NativePendingEventKind::Mouse => {
            msg_send![view, respondsToSelector: sel!(_doAfterProcessingAllPendingMouseEvents:)]
        }
        NativePendingEventKind::Key => {
            msg_send![view, respondsToSelector: sel!(_doAfterProcessingAllPendingKeyEvents:)]
        }
        NativePendingEventKind::None => true,
    };
    if supported {
        Ok(())
    } else {
        Err(NativeInputFailure::Typed {
            status: tidebreak_core::BrowserActStatus::UnsupportedNative,
            message: "This WebKit version cannot wait for browser presentation.".to_owned(),
        })
    }
}

#[cfg(target_os = "macos")]
async fn wait_for_native_action_processing(
    webview: &Webview,
    action: &tidebreak_core::BrowserAction,
    phase: NativeActionDispatchPhase,
) -> Result<(), NativeInputFailure> {
    use std::sync::{Arc, Mutex};

    use block2::RcBlock;
    use objc2::{msg_send, sel};
    use tokio::{sync::oneshot, time::timeout};

    let kind = native_pending_event_kind(action, phase);
    if matches!(kind, NativePendingEventKind::None) {
        return Ok(());
    }

    let (sender, receiver) = oneshot::channel::<Result<(), NativeInputFailure>>();
    let sender = Arc::new(Mutex::new(Some(sender)));
    let callback_sender = Arc::clone(&sender);
    with_browser_webview(webview, move |view| unsafe {
        let supported = match kind {
            NativePendingEventKind::Presentation => {
                msg_send![view, respondsToSelector: sel!(_doAfterNextPresentationUpdate:)]
            }
            NativePendingEventKind::Mouse => {
                msg_send![view, respondsToSelector: sel!(_doAfterProcessingAllPendingMouseEvents:)]
            }
            NativePendingEventKind::Key => {
                msg_send![view, respondsToSelector: sel!(_doAfterProcessingAllPendingKeyEvents:)]
            }
            NativePendingEventKind::None => true,
        };
        if !supported {
            if let Some(sender) = sender.lock().ok().and_then(|mut sender| sender.take()) {
                let outcome = if matches!(kind, NativePendingEventKind::Presentation) {
                    Err(NativeInputFailure::Typed {
                        status: tidebreak_core::BrowserActStatus::UnsupportedNative,
                        message: "This WebKit version cannot wait for browser presentation."
                            .to_owned(),
                    })
                } else {
                    Ok(())
                };
                let _ = sender.send(outcome);
            }
            return;
        }
        let handler = RcBlock::new(move || {
            if let Some(sender) = callback_sender
                .lock()
                .ok()
                .and_then(|mut sender| sender.take())
            {
                let _ = sender.send(Ok(()));
            }
        });
        match kind {
            NativePendingEventKind::Presentation => {
                let _: () = msg_send![view, _doAfterNextPresentationUpdate: &*handler];
            }
            NativePendingEventKind::Mouse => {
                let _: () = msg_send![
                    view,
                    _doAfterProcessingAllPendingMouseEvents: &*handler
                ];
            }
            NativePendingEventKind::Key => {
                let _: () = msg_send![
                    view,
                    _doAfterProcessingAllPendingKeyEvents: &*handler
                ];
            }
            NativePendingEventKind::None => {}
        }
    })
    .map_err(NativeInputFailure::Engine)?;

    timeout(
        std::time::Duration::from_secs(NATIVE_EVENT_PROCESSING_TIMEOUT_SECONDS),
        receiver,
    )
    .await
    .map_err(|_| {
        NativeInputFailure::Timeout("native browser input did not finish processing".to_owned())
    })?
    .map_err(|_| NativeInputFailure::Engine("native browser input was interrupted".to_owned()))??;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
async fn wait_for_native_action_processing(
    _webview: &Webview,
    _action: &tidebreak_core::BrowserAction,
    _phase: NativeActionDispatchPhase,
) -> Result<(), NativeInputFailure> {
    Ok(())
}

fn native_input_deadline_failure(
    phase: NativeActionDispatchPhase,
    phase_deadline: Option<tokio::time::Instant>,
) -> NativeInputFailure {
    if phase_deadline.is_some() {
        native_action_postcondition_timeout(phase)
    } else {
        NativeInputFailure::Timeout("native browser input timed out".to_owned())
    }
}

fn native_action_postcondition_timeout(phase: NativeActionDispatchPhase) -> NativeInputFailure {
    match phase {
        NativeActionDispatchPhase::SelectFollowUp { .. } => NativeInputFailure::Engine(
            "The browser did not move the select control toward the requested option.".to_owned(),
        ),

        NativeActionDispatchPhase::FocusVerify
        | NativeActionDispatchPhase::PressKey
        | NativeActionDispatchPhase::SelectInitialStep => NativeInputFailure::Typed {
            status: tidebreak_core::BrowserActStatus::UnsupportedNative,
            message: "The target did not retain native focus. No key was sent.".to_owned(),
        },
        NativeActionDispatchPhase::FillVerify => NativeInputFailure::Engine(
            "The browser did not retain the requested field value.".to_owned(),
        ),
        _ => NativeInputFailure::Typed {
            status: tidebreak_core::BrowserActStatus::UnsupportedNative,
            message:
                "The target did not retain native focus or text selection. No text was inserted."
                    .to_owned(),
        },
    }
}

async fn wait_for_native_action_phase<F, Fut>(
    phase: NativeActionDispatchPhase,
    timeout: std::time::Duration,
    mut resolve: F,
) -> Result<NativeActionResolution, NativeInputFailure>
where
    F: FnMut(tokio::time::Instant) -> Fut,
    Fut: std::future::Future<Output = Result<NativeActionResolution, NativeInputFailure>>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(native_action_postcondition_timeout(phase));
        }
        // The native resolver owns this deadline. Its callback mutex preserves
        // the result when input has already dispatched before timeout fires.
        let resolution = resolve(deadline).await?;
        if resolution.status != NativeActionResolutionStatus::PendingNativeInput {
            return Ok(resolution);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(native_action_postcondition_timeout(phase));
        }
        // Pending resolution does not dispatch input. Resolve the same private
        // target again, including its live authority, before the next phase.
        tokio::time::sleep_until(std::cmp::min(
            deadline,
            tokio::time::Instant::now()
                + std::time::Duration::from_millis(NATIVE_ACTION_VERIFY_DELAY_MILLIS),
        ))
        .await;
    }
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
async fn finish_native_action(
    webview: &Webview,
    registry: &BrowserRegistry,
    capability_id: Uuid,
    workspace_id: &str,
    origin: &BrowserOrigin,
    fence: BrowserObservationFence,
    arguments: &tidebreak_core::BrowserActArgs,
    initial_resolution: &NativeActionResolution,
    target: &BrowserTargetRecord,
) -> Result<(), NativeInputFailure> {
    let resolution_script = native_action_resolution_script(target, &arguments.action)
        .map_err(NativeInputFailure::Engine)?;
    match &arguments.action {
        tidebreak_core::BrowserAction::Focus | tidebreak_core::BrowserAction::Press { .. } => {
            let phase = if matches!(arguments.action, tidebreak_core::BrowserAction::Focus) {
                NativeActionDispatchPhase::FocusVerify
            } else {
                NativeActionDispatchPhase::PressKey
            };
            let script =
                native_action_resolution_script_for_phase(target, &arguments.action, phase)
                    .map_err(NativeInputFailure::Engine)?;
            let resolution = wait_for_native_action_phase(
                phase,
                std::time::Duration::from_secs(NATIVE_EVENT_PROCESSING_TIMEOUT_SECONDS),
                |deadline| {
                    continue_native_action(
                        webview,
                        registry,
                        capability_id,
                        workspace_id,
                        origin,
                        fence,
                        arguments,
                        &script,
                        phase,
                        Some(deadline),
                    )
                },
            )
            .await?;
            match resolution.status {
                NativeActionResolutionStatus::NoOp => Ok(()),
                NativeActionResolutionStatus::Ready => {
                    wait_for_native_action_processing(webview, &arguments.action, phase).await
                }
                _ => Err(native_resolution_failure(resolution)),
            }
        }

        tidebreak_core::BrowserAction::Fill { .. } => {
            for phase in [
                NativeActionDispatchPhase::FillSelectAll,
                NativeActionDispatchPhase::FillInsert,
                NativeActionDispatchPhase::FillVerify,
            ] {
                let script =
                    native_action_resolution_script_for_phase(target, &arguments.action, phase)
                        .map_err(NativeInputFailure::Engine)?;
                let resolution = wait_for_native_action_phase(
                    phase,
                    std::time::Duration::from_secs(NATIVE_EVENT_PROCESSING_TIMEOUT_SECONDS),
                    |deadline| {
                        continue_native_action(
                            webview,
                            registry,
                            capability_id,
                            workspace_id,
                            origin,
                            fence,
                            arguments,
                            &script,
                            phase,
                            Some(deadline),
                        )
                    },
                )
                .await?;
                match resolution.status {
                    NativeActionResolutionStatus::NoOp => return Ok(()),
                    NativeActionResolutionStatus::Ready => {
                        wait_for_native_action_processing(webview, &arguments.action, phase)
                            .await?;
                    }
                    _ => return Err(native_resolution_failure(resolution)),
                }
            }
            Err(NativeInputFailure::Engine(
                "The browser could not confirm the requested field value.".to_owned(),
            ))
        }
        tidebreak_core::BrowserAction::Select { .. } => {
            let resolution_script = native_action_resolution_script_for_phase(
                target,
                &arguments.action,
                NativeActionDispatchPhase::SelectInitialStep,
            )
            .map_err(NativeInputFailure::Engine)?;
            let initial_resolution = wait_for_native_action_phase(
                NativeActionDispatchPhase::SelectInitialStep,
                std::time::Duration::from_secs(NATIVE_EVENT_PROCESSING_TIMEOUT_SECONDS),
                |deadline| {
                    continue_native_action(
                        webview,
                        registry,
                        capability_id,
                        workspace_id,
                        origin,
                        fence,
                        arguments,
                        &resolution_script,
                        NativeActionDispatchPhase::SelectInitialStep,
                        Some(deadline),
                    )
                },
            )
            .await?;
            match initial_resolution.status {
                NativeActionResolutionStatus::NoOp => return Ok(()),
                NativeActionResolutionStatus::Ready => {
                    wait_for_native_action_processing(
                        webview,
                        &arguments.action,
                        NativeActionDispatchPhase::SelectInitialStep,
                    )
                    .await?
                }
                _ => return Err(native_resolution_failure(initial_resolution)),
            }

            let mut selected_index = initial_resolution.selected_index.ok_or_else(|| {
                NativeInputFailure::Engine("browser select has no current option index".to_owned())
            })?;
            let option_index = initial_resolution.option_index.ok_or_else(|| {
                NativeInputFailure::Engine(
                    "browser select has no requested option index".to_owned(),
                )
            })?;
            let mut distance = selected_index.abs_diff(option_index);
            for _ in 0..=MAX_NATIVE_SELECT_STEPS {
                let phase = NativeActionDispatchPhase::SelectFollowUp {
                    previous_selected_index: selected_index,
                    previous_distance: distance,
                };
                let script =
                    native_action_resolution_script_for_phase(target, &arguments.action, phase)
                        .map_err(NativeInputFailure::Engine)?;
                let resolution = wait_for_native_action_phase(
                    phase,
                    std::time::Duration::from_secs(NATIVE_EVENT_PROCESSING_TIMEOUT_SECONDS),
                    |deadline| {
                        continue_native_action(
                            webview,
                            registry,
                            capability_id,
                            workspace_id,
                            origin,
                            fence,
                            arguments,
                            &script,
                            phase,
                            Some(deadline),
                        )
                    },
                )
                .await?;
                match resolution.status {
                    NativeActionResolutionStatus::NoOp => return Ok(()),
                    NativeActionResolutionStatus::Ready => {
                        selected_index = resolution.selected_index.ok_or_else(|| {
                            NativeInputFailure::Engine(
                                "browser select has no current option index".to_owned(),
                            )
                        })?;
                        distance = selected_index.abs_diff(option_index);
                        wait_for_native_action_processing(
                            webview,
                            &arguments.action,
                            NativeActionDispatchPhase::Initial,
                        )
                        .await?;
                    }
                    _ => return Err(native_resolution_failure(resolution)),
                }
            }
            Err(NativeInputFailure::Engine(
                "The browser could not confirm the requested select option.".to_owned(),
            ))
        }
        tidebreak_core::BrowserAction::ScrollIntoView => {
            let mut previous = initial_resolution.clone();
            for _ in 0..=MAX_NATIVE_SCROLL_STEPS {
                let resolution = continue_native_action(
                    webview,
                    registry,
                    capability_id,
                    workspace_id,
                    origin,
                    fence,
                    arguments,
                    &resolution_script,
                    NativeActionDispatchPhase::ScrollFollowUp {
                        previous_x: previous.x.unwrap_or_default(),
                        previous_y: previous.y.unwrap_or_default(),
                        previous_delta_x: previous.scroll_delta_x.unwrap_or_default(),
                        previous_delta_y: previous.scroll_delta_y.unwrap_or_default(),
                    },
                    None,
                )
                .await?;
                match resolution.status {
                    NativeActionResolutionStatus::NoOp => return Ok(()),
                    NativeActionResolutionStatus::Ready => {
                        previous = resolution;
                        wait_for_native_action_processing(
                            webview,
                            &arguments.action,
                            NativeActionDispatchPhase::Initial,
                        )
                        .await?;
                    }
                    _ => return Err(native_resolution_failure(resolution)),
                }
            }
            Err(NativeInputFailure::Typed {
                status: tidebreak_core::BrowserActStatus::TargetObscured,
                message: "The browser could not scroll the target into the visible viewport."
                    .to_owned(),
            })
        }
        _ => Ok(()),
    }
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
async fn continue_native_action(
    webview: &Webview,
    registry: &BrowserRegistry,
    capability_id: Uuid,
    workspace_id: &str,
    origin: &BrowserOrigin,
    fence: BrowserObservationFence,
    arguments: &tidebreak_core::BrowserActArgs,
    resolution_script: &str,
    phase: NativeActionDispatchPhase,
    phase_deadline: Option<tokio::time::Instant>,
) -> Result<NativeActionResolution, NativeInputFailure> {
    tokio::time::sleep(std::time::Duration::from_millis(
        NATIVE_ACTION_VERIFY_DELAY_MILLIS,
    ))
    .await;
    match resolve_and_dispatch_native_action(
        webview,
        registry.clone(),
        capability_id,
        workspace_id.to_owned(),
        origin.clone(),
        fence,
        arguments,
        resolution_script.to_owned(),
        phase,
        phase_deadline,
    )
    .await?
    {
        NativeActionDispatchOutcome::Resolved {
            resolution,
            dispatch_error,
        } => {
            if let Some(failure) = dispatch_error {
                return Err(failure);
            }
            Ok(*resolution)
        }
        NativeActionDispatchOutcome::TargetRejected(error) => Err(NativeInputFailure::Typed {
            status: act_status_from_target_error(error),
            message: target_error_message(error).to_owned(),
        }),
    }
}

#[cfg(not(target_os = "macos"))]
#[allow(clippy::too_many_arguments)]
async fn resolve_and_dispatch_native_action(
    _webview: &Webview,
    _registry: BrowserRegistry,
    _capability_id: Uuid,
    _workspace_id: String,
    _origin: BrowserOrigin,
    _fence: BrowserObservationFence,
    _arguments: &tidebreak_core::BrowserActArgs,
    _script: String,
    _phase: NativeActionDispatchPhase,
    _phase_deadline: Option<tokio::time::Instant>,
) -> Result<NativeActionDispatchOutcome, NativeInputFailure> {
    Err(NativeInputFailure::Engine(
        "trusted native browser input is not available on this platform".to_owned(),
    ))
}

#[cfg(not(target_os = "macos"))]
#[allow(clippy::too_many_arguments)]
async fn finish_native_action(
    _webview: &Webview,
    _registry: &BrowserRegistry,
    _capability_id: Uuid,
    _workspace_id: &str,
    _origin: &BrowserOrigin,
    _fence: BrowserObservationFence,
    _arguments: &tidebreak_core::BrowserActArgs,
    _initial_resolution: &NativeActionResolution,
    _target: &BrowserTargetRecord,
) -> Result<(), NativeInputFailure> {
    Ok(())
}

#[cfg(target_os = "macos")]
unsafe fn dispatch_native_action(
    view: &objc2_web_kit::WKWebView,
    resolution: &NativeActionResolution,
    action: &tidebreak_core::BrowserAction,
    phase: NativeActionDispatchPhase,
) -> Result<(), NativeInputFailure> {
    use objc2_app_kit::NSView;

    let native_view: &NSView = &*(view as *const _ as *const NSView);
    let window = native_view
        .window()
        .ok_or_else(|| NativeInputFailure::Engine("browser window is not available".to_owned()))?;
    ensure_active_browser_window(&window)?;
    let window_point =
        native_window_point(native_view, resolution).map_err(NativeInputFailure::Engine)?;
    let screen_point = window.convertPointToScreen(window_point);

    match action {
        tidebreak_core::BrowserAction::Click | tidebreak_core::BrowserAction::Check { .. } => {
            send_native_click(&window, window_point).map_err(NativeInputFailure::Engine)?;
        }
        tidebreak_core::BrowserAction::Hover => {
            send_native_hover(view, &window, window_point).map_err(NativeInputFailure::Engine)?;
        }
        tidebreak_core::BrowserAction::Focus => {
            acquire_native_target_focus(view, &window, screen_point, resolution)?;
        }
        tidebreak_core::BrowserAction::Fill { value } => match phase {
            NativeActionDispatchPhase::Initial => {
                ensure_native_event_processing_supported(
                    view,
                    NativePendingEventKind::Presentation,
                )?;
                send_native_click(&window, window_point).map_err(NativeInputFailure::Engine)?;
            }
            NativeActionDispatchPhase::FillSelectAll | NativeActionDispatchPhase::FillInsert => {
                ensure_native_event_processing_supported(
                    view,
                    NativePendingEventKind::Presentation,
                )?;
                insert_native_text(view, &window, value, phase)?;
            }
            _ => {
                return Err(NativeInputFailure::Engine(
                    "browser fill reached an invalid native input phase".to_owned(),
                ));
            }
        },
        tidebreak_core::BrowserAction::Press { key } => {
            if matches!(phase, NativeActionDispatchPhase::Initial) {
                acquire_native_target_focus(view, &window, screen_point, resolution)?;
            } else {
                native_browser_responder(view, &window)?;
                send_native_key(&window, window_point, key).map_err(NativeInputFailure::Engine)?;
            }
        }
        tidebreak_core::BrowserAction::Select { .. } => {
            if matches!(phase, NativeActionDispatchPhase::Initial) {
                acquire_native_target_focus(view, &window, screen_point, resolution)?;
            } else {
                native_browser_responder(view, &window)?;
                send_native_select_step(&window, window_point, resolution)?;
            }
        }
        tidebreak_core::BrowserAction::ScrollIntoView => {
            send_native_scroll(view, &window, resolution).map_err(NativeInputFailure::Engine)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
unsafe fn native_browser_responder(
    view: &objc2_web_kit::WKWebView,
    window: &objc2_app_kit::NSWindow,
) -> Result<objc2::rc::Retained<objc2_app_kit::NSResponder>, NativeInputFailure> {
    use objc2::{msg_send, ClassType};
    use objc2_app_kit::NSView;
    let responder = window.firstResponder().ok_or_else(|| {
        NativeInputFailure::Engine("browser input target did not accept focus".to_owned())
    })?;
    let is_view: bool = msg_send![&*responder, isKindOfClass: NSView::class()];
    if !is_view {
        return Err(NativeInputFailure::Engine(
            "browser keyboard focus moved outside the page".to_owned(),
        ));
    }
    let native_view: &NSView = &*(view as *const _ as *const NSView);
    let responder_view: &NSView = &*(&*responder as *const _ as *const NSView);
    if !std::ptr::eq(responder_view, native_view) && !responder_view.isDescendantOf(native_view) {
        return Err(NativeInputFailure::Engine(
            "browser keyboard focus moved outside the page".to_owned(),
        ));
    }
    Ok(responder)
}

#[cfg(target_os = "macos")]
unsafe fn verify_native_page_focus(
    view: &objc2_web_kit::WKWebView,
) -> Result<(), NativeInputFailure> {
    let native_view: &objc2_app_kit::NSView = &*(view as *const _ as *const objc2_app_kit::NSView);
    let window = native_view
        .window()
        .ok_or_else(|| NativeInputFailure::Engine("browser window is not available".to_owned()))?;
    ensure_active_browser_window(&window)?;
    native_browser_responder(view, &window)?;
    Ok(())
}

#[cfg(target_os = "macos")]
unsafe fn acquire_native_target_focus(
    view: &objc2_web_kit::WKWebView,
    window: &objc2_app_kit::NSWindow,
    screen_point: objc2_foundation::NSPoint,
    resolution: &NativeActionResolution,
) -> Result<(), NativeInputFailure> {
    ensure_native_event_processing_supported(view, NativePendingEventKind::Presentation)?;
    if resolution.target_dom_focused {
        if resolution.target_focused && native_browser_responder(view, window).is_ok() {
            return Ok(());
        }
        let responder: &objc2_app_kit::NSResponder =
            &*(view as *const _ as *const objc2_app_kit::NSResponder);
        if !window.makeFirstResponder(Some(responder)) {
            return Err(NativeInputFailure::Typed {
                status: tidebreak_core::BrowserActStatus::UnsupportedNative,
                message: "The browser view did not accept native keyboard focus.".to_owned(),
            });
        }
        return Ok(());
    }
    focus_accessibility_target(view, window, screen_point).map_err(|_| NativeInputFailure::Typed {
        status: tidebreak_core::BrowserActStatus::UnsupportedNative,
        message: "This target cannot receive native accessibility focus. Focus it in the browser, then take a new snapshot.".to_owned(),
    })?;
    Ok(())
}

#[cfg(target_os = "macos")]
unsafe fn insert_native_text(
    view: &objc2_web_kit::WKWebView,
    window: &objc2_app_kit::NSWindow,
    value: &str,
    phase: NativeActionDispatchPhase,
) -> Result<(), NativeInputFailure> {
    use objc2_app_kit::NSStandardKeyBindingResponding;
    let responder = native_browser_responder(view, window)?;
    match phase {
        NativeActionDispatchPhase::FillSelectAll => responder.selectAll(None),
        NativeActionDispatchPhase::FillInsert => {
            let text = objc2_foundation::NSString::from_str(value);
            responder.insertText(&text);
        }
        _ => {
            return Err(NativeInputFailure::Engine(
                "browser fill reached an invalid text phase".to_owned(),
            ))
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
unsafe fn ensure_active_browser_window(
    window: &objc2_app_kit::NSWindow,
) -> Result<(), NativeInputFailure> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    let app = NSApplication::sharedApplication(MainThreadMarker::new_unchecked());
    if !app.isActive() || !window.isVisible() || window.isMiniaturized() || !window.isKeyWindow() {
        return Err(NativeInputFailure::HiddenTab(
            "Bring Tidebreak and this browser tab to the foreground before acting.".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn native_window_point(
    view: &objc2_app_kit::NSView,
    resolution: &NativeActionResolution,
) -> Result<objc2_foundation::NSPoint, String> {
    let x = resolution
        .x
        .ok_or_else(|| "browser target has no horizontal position".to_owned())?;
    let y = resolution
        .y
        .ok_or_else(|| "browser target has no vertical position".to_owned())?;
    let width = resolution
        .width
        .ok_or_else(|| "browser target has no width".to_owned())?;
    let height = resolution
        .height
        .ok_or_else(|| "browser target has no height".to_owned())?;
    let viewport_width = resolution
        .viewport_width
        .filter(|value| *value > 0.0)
        .ok_or_else(|| "browser viewport has no width".to_owned())?;
    let viewport_height = resolution
        .viewport_height
        .filter(|value| *value > 0.0)
        .ok_or_else(|| "browser viewport has no height".to_owned())?;
    native_window_point_for_css(
        view,
        x + width / 2.0,
        y + height / 2.0,
        viewport_width,
        viewport_height,
    )
}

#[cfg(target_os = "macos")]
fn native_window_point_for_css(
    view: &objc2_app_kit::NSView,
    x: f64,
    y: f64,
    viewport_width: f64,
    viewport_height: f64,
) -> Result<objc2_foundation::NSPoint, String> {
    if !x.is_finite()
        || !y.is_finite()
        || !viewport_width.is_finite()
        || !viewport_height.is_finite()
        || viewport_width <= 0.0
        || viewport_height <= 0.0
    {
        return Err("browser native input coordinates are not valid".to_owned());
    }
    let bounds = view.bounds();
    let local_x = bounds.origin.x + x * bounds.size.width / viewport_width;
    let from_top = y * bounds.size.height / viewport_height;
    let local_y = if view.isFlipped() {
        bounds.origin.y + from_top
    } else {
        bounds.origin.y + bounds.size.height - from_top
    };
    Ok(view.convertPoint_toView(objc2_foundation::NSPoint::new(local_x, local_y), None))
}

#[cfg(target_os = "macos")]
fn send_native_click(
    window: &objc2_app_kit::NSWindow,
    point: objc2_foundation::NSPoint,
) -> Result<(), String> {
    use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};

    let timestamp = native_event_timestamp();
    let down = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType::LeftMouseDown,
        point,
        NSEventModifierFlags::empty(),
        timestamp,
        window.windowNumber(),
        None,
        0,
        1,
        1.0,
    )
    .ok_or_else(|| "could not create native mouse-down input".to_owned())?;
    let up = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType::LeftMouseUp,
        point,
        NSEventModifierFlags::empty(),
        timestamp,
        window.windowNumber(),
        None,
        0,
        1,
        0.0,
    )
    .ok_or_else(|| "could not create native mouse-up input".to_owned())?;
    window.sendEvent(&down);
    window.sendEvent(&up);
    Ok(())
}

#[cfg(target_os = "macos")]
fn send_native_hover(
    view: &objc2_web_kit::WKWebView,
    window: &objc2_app_kit::NSWindow,
    point: objc2_foundation::NSPoint,
) -> Result<(), String> {
    use objc2::{msg_send, runtime::AnyObject, sel, MainThreadMarker};
    use objc2_app_kit::{
        NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSGraphicsContext, NSResponder,
    };

    let app = NSApplication::sharedApplication(unsafe { MainThreadMarker::new_unchecked() });
    let responder: &NSResponder = unsafe { &*(view as *const _ as *const NSResponder) };
    if !window.makeFirstResponder(Some(responder)) {
        return Err("browser view did not accept pointer focus".to_owned());
    }
    window.setAcceptsMouseMovedEvents(true);
    let context = NSGraphicsContext::currentContext();
    let timestamp = native_event_timestamp();
    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType::MouseMoved,
        point,
        NSEventModifierFlags::empty(),
        timestamp,
        window.windowNumber(),
        context.as_deref(),
        0,
        0,
        0.0,
    )
    .ok_or_else(|| "could not create native pointer-move input".to_owned())?;
    window.sendEvent(&event);
    let can_simulate: bool =
        unsafe { msg_send![view, respondsToSelector: sel!(_simulateMouseMove:)] };
    if !can_simulate {
        return Err("this macOS WebKit version cannot synthesize native hover input".to_owned());
    }
    let can_set_current: bool =
        unsafe { msg_send![&*app, respondsToSelector: sel!(_setCurrentEvent:)] };
    if can_set_current {
        let _: () = unsafe { msg_send![&*app, _setCurrentEvent: &*event] };
    }
    let _: () = unsafe { msg_send![view, _simulateMouseMove: &*event] };
    if can_set_current {
        let _: () = unsafe {
            msg_send![
                &*app,
                _setCurrentEvent: std::ptr::null_mut::<AnyObject>()
            ]
        };
    }
    Ok(())
}

#[cfg(target_os = "macos")]
unsafe fn focus_accessibility_target(
    view: &objc2_web_kit::WKWebView,
    window: &objc2_app_kit::NSWindow,
    screen_point: objc2_foundation::NSPoint,
) -> Result<objc2::rc::Retained<objc2::runtime::AnyObject>, String> {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{msg_send, sel};
    use objc2_app_kit::NSResponder;

    let target: Option<Retained<AnyObject>> = msg_send![view, accessibilityHitTest: screen_point];
    let target =
        target.ok_or_else(|| "browser target is not exposed to native accessibility".to_owned())?;
    let supports_focus: bool =
        msg_send![&*target, respondsToSelector: sel!(setAccessibilityFocused:)];
    if !supports_focus {
        return Err("browser target cannot accept native accessibility focus".to_owned());
    }
    let responder: &NSResponder = &*(view as *const _ as *const NSResponder);
    if !window.makeFirstResponder(Some(responder)) {
        return Err("browser view did not accept keyboard focus".to_owned());
    }
    let _: () = msg_send![&*target, setAccessibilityFocused: true];
    ensure_accessibility_target_focused(&target)?;
    Ok(target)
}

#[cfg(target_os = "macos")]
unsafe fn ensure_accessibility_target_focused(
    target: &objc2::runtime::AnyObject,
) -> Result<(), String> {
    use objc2::{msg_send, sel};

    let supports_focus_state: bool =
        msg_send![target, respondsToSelector: sel!(isAccessibilityFocused)];
    if !supports_focus_state {
        return Err("browser target cannot confirm native accessibility focus".to_owned());
    }
    let focused: bool = msg_send![target, isAccessibilityFocused];
    if !focused {
        return Err("browser input target did not accept accessibility focus".to_owned());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
unsafe fn send_native_select_step(
    window: &objc2_app_kit::NSWindow,
    window_point: objc2_foundation::NSPoint,
    resolution: &NativeActionResolution,
) -> Result<(), NativeInputFailure> {
    let selected_index = resolution.selected_index.ok_or_else(|| {
        NativeInputFailure::Engine("browser select has no current option index".to_owned())
    })?;
    let option_index = resolution.option_index.ok_or_else(|| {
        NativeInputFailure::Engine("browser select has no requested option index".to_owned())
    })?;
    let distance = selected_index.abs_diff(option_index);
    if distance == 0 {
        return Err(NativeInputFailure::Engine(
            "browser select resolution did not require native input".to_owned(),
        ));
    }
    if distance > MAX_NATIVE_SELECT_STEPS as u64 {
        return Err(NativeInputFailure::Typed {
            status: tidebreak_core::BrowserActStatus::UnsupportedNative,
            message: "The requested option is too far from the current selection for bounded native input."
                .to_owned(),
        });
    }
    let key = if option_index > selected_index {
        "ArrowDown"
    } else {
        "ArrowUp"
    };
    send_native_key(window, window_point, key).map_err(NativeInputFailure::Engine)
}

#[cfg(target_os = "macos")]
fn send_native_key(
    window: &objc2_app_kit::NSWindow,
    point: objc2_foundation::NSPoint,
    key: &str,
) -> Result<(), String> {
    use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};

    let (key_code, character) = native_key(key)?;
    let characters = objc2_foundation::NSString::from_str(&character);
    let timestamp = native_event_timestamp();
    for event_type in [NSEventType::KeyDown, NSEventType::KeyUp] {
        let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            event_type,
            point,
            NSEventModifierFlags::empty(),
            timestamp,
            window.windowNumber(),
            None,
            &characters,
            &characters,
            false,
            key_code,
        )
        .ok_or_else(|| format!("could not create native {key} key input"))?;
        window.sendEvent(&event);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn native_event_timestamp() -> f64 {
    const APPLE_REFERENCE_DATE_UNIX_SECONDS: f64 = 978_307_200.0;
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |duration| {
            duration.as_secs_f64() - APPLE_REFERENCE_DATE_UNIX_SECONDS
        })
}

#[cfg(target_os = "macos")]
fn native_key(key: &str) -> Result<(u16, String), String> {
    use objc2_app_kit::{
        NSDeleteFunctionKey, NSDownArrowFunctionKey, NSLeftArrowFunctionKey,
        NSRightArrowFunctionKey, NSUpArrowFunctionKey,
    };

    let mapping = match key {
        "Enter" => (0x24, '\r'.to_string()),
        "Escape" => (0x35, '\u{1b}'.to_string()),
        "Tab" => (0x30, '\t'.to_string()),
        " " => (0x31, " ".to_owned()),
        "ArrowUp" => (0x7e, native_function_character(NSUpArrowFunctionKey)?),
        "ArrowDown" => (0x7d, native_function_character(NSDownArrowFunctionKey)?),
        "ArrowLeft" => (0x7b, native_function_character(NSLeftArrowFunctionKey)?),
        "ArrowRight" => (0x7c, native_function_character(NSRightArrowFunctionKey)?),
        "Backspace" => (0x33, '\u{8}'.to_string()),
        "Delete" => (0x75, native_function_character(NSDeleteFunctionKey)?),
        _ => return Err("browser key is not supported by the native adapter".to_owned()),
    };
    Ok(mapping)
}

#[cfg(target_os = "macos")]
fn native_function_character(value: u32) -> Result<String, String> {
    char::from_u32(value)
        .map(|character| character.to_string())
        .ok_or_else(|| "native browser key has an invalid character".to_owned())
}

#[cfg(target_os = "macos")]
fn send_native_scroll(
    view: &objc2_web_kit::WKWebView,
    window: &objc2_app_kit::NSWindow,
    resolution: &NativeActionResolution,
) -> Result<(), String> {
    use objc2::{msg_send, rc::Retained, sel};
    use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType, NSGraphicsContext, NSView};
    use objc2_core_graphics::{
        CGEvent, CGEventField, CGEventSource, CGEventSourceStateID, CGScrollEventUnit,
    };

    let scroll_x = resolution
        .scroll_x
        .ok_or_else(|| "browser scroll target has no horizontal position".to_owned())?;
    let scroll_y = resolution
        .scroll_y
        .ok_or_else(|| "browser scroll target has no vertical position".to_owned())?;
    let viewport_width = resolution
        .viewport_width
        .filter(|value| *value > 0.0)
        .ok_or_else(|| "browser viewport has no width".to_owned())?;
    let viewport_height = resolution
        .viewport_height
        .filter(|value| *value > 0.0)
        .ok_or_else(|| "browser viewport has no height".to_owned())?;
    let delta_x = resolution.scroll_delta_x.unwrap_or_default();
    let delta_y = resolution.scroll_delta_y.unwrap_or_default();
    if delta_x.abs() < 1.0 && delta_y.abs() < 1.0 {
        return Err("browser scroll plan did not require native input".to_owned());
    }
    let native_view: &NSView = unsafe { &*(view as *const _ as *const NSView) };
    let window_point = native_window_point_for_css(
        native_view,
        scroll_x,
        scroll_y,
        viewport_width,
        viewport_height,
    )?;
    let context = NSGraphicsContext::currentContext();
    let location_event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType::MouseMoved,
        window_point,
        NSEventModifierFlags::empty(),
        native_event_timestamp(),
        window.windowNumber(),
        context.as_deref(),
        0,
        0,
        0.0,
    )
    .ok_or_else(|| "could not create native scroll location".to_owned())?;
    let location_cg_event = location_event
        .CGEvent()
        .ok_or_else(|| "could not resolve native scroll location".to_owned())?;
    let screen_location = CGEvent::location(Some(&location_cg_event));
    let source = CGEventSource::new(CGEventSourceStateID::Private)
        .ok_or_else(|| "could not create native scroll source".to_owned())?;
    let bounds = native_view.bounds();
    let (changed_x, changed_y) = native_scroll_delta_for_css(
        delta_x,
        delta_y,
        bounds.size.width,
        bounds.size.height,
        viewport_width,
        viewport_height,
    )?;
    let supports_relative_event: bool =
        unsafe { msg_send![&*location_event, respondsToSelector: sel!(_eventRelativeToWindow:)] };
    if !supports_relative_event {
        return Err("this macOS version cannot target native scroll input".to_owned());
    }
    for (phase, wheel_y, wheel_x) in native_scroll_event_steps(changed_x, changed_y) {
        let cg_event = CGEvent::new_scroll_wheel_event2(
            Some(&source),
            CGScrollEventUnit::Pixel,
            2,
            wheel_y,
            wheel_x,
            0,
        )
        .ok_or_else(|| "could not create native scroll input".to_owned())?;
        CGEvent::set_location(Some(&cg_event), screen_location);
        CGEvent::set_integer_value_field(
            Some(&cg_event),
            CGEventField::ScrollWheelEventScrollPhase,
            phase,
        );
        CGEvent::set_integer_value_field(
            Some(&cg_event),
            CGEventField::ScrollWheelEventIsContinuous,
            1,
        );
        let event = NSEvent::eventWithCGEvent(&cg_event)
            .ok_or_else(|| "could not bridge native scroll input".to_owned())?;
        let relative_event: Option<Retained<NSEvent>> =
            unsafe { msg_send![&*event, _eventRelativeToWindow: window] };
        let relative_event = relative_event.ok_or_else(|| {
            "could not attach native scroll input to the browser window".to_owned()
        })?;
        window.sendEvent(&relative_event);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn native_scroll_delta_for_css(
    delta_x: f64,
    delta_y: f64,
    native_width: f64,
    native_height: f64,
    viewport_width: f64,
    viewport_height: f64,
) -> Result<(i32, i32), String> {
    if !delta_x.is_finite()
        || !delta_y.is_finite()
        || !native_width.is_finite()
        || !native_height.is_finite()
        || !viewport_width.is_finite()
        || !viewport_height.is_finite()
        || native_width <= 0.0
        || native_height <= 0.0
        || viewport_width <= 0.0
        || viewport_height <= 0.0
    {
        return Err("browser native scroll dimensions are not valid".to_owned());
    }
    let changed_x = (-delta_x * native_width / viewport_width)
        .round()
        .clamp(i32::MIN as f64, i32::MAX as f64) as i32;
    let changed_y = (-delta_y * native_height / viewport_height)
        .round()
        .clamp(i32::MIN as f64, i32::MAX as f64) as i32;
    Ok((changed_x, changed_y))
}

#[cfg(target_os = "macos")]
fn native_scroll_event_steps(changed_x: i32, changed_y: i32) -> [(i64, i32, i32); 3] {
    [(1, 0, 0), (2, changed_y, changed_x), (4, 0, 0)]
}

fn act_result(
    request: &tidebreak_core::BrowserActArgs,
    status: tidebreak_core::BrowserActStatus,
    message: &str,
) -> tidebreak_core::BrowserActResult {
    use tidebreak_core::BrowserActResult;
    BrowserActResult {
        browser_id: request.browser_id.clone(),
        snapshot_id: request.snapshot_id.clone(),
        document_epoch: request.document_epoch,
        target_ref: request.target_ref.clone(),
        action: request.action.kind().to_owned(),
        status,
        message: message.to_owned(),
        requires_resnapshot: matches!(status, tidebreak_core::BrowserActStatus::StaleTarget),
        url: None,
        title: None,
    }
}

/// Map a [`BrowserTargetError`] to the corresponding typed status.
pub(crate) fn act_status_from_target_error(
    error: BrowserTargetError,
) -> tidebreak_core::BrowserActStatus {
    use tidebreak_core::BrowserActStatus;
    match error {
        BrowserTargetError::StaleTarget => BrowserActStatus::StaleTarget,
        BrowserTargetError::BrowserHidden => BrowserActStatus::HiddenTab,
    }
}

fn snapshot_script(max_nodes: usize, marker: &str) -> String {
    SNAPSHOT_SCRIPT
        .replace("__MAX_NODES__", &max_nodes.to_string())
        .replace("__MARKER__", marker)
        .replace("__TARGET_IDENTITY_STORE__", TARGET_IDENTITY_STORE_SCRIPT)
        .replace("__SENSITIVE_FIELD_POLICY__", SENSITIVE_FIELD_POLICY)
}

fn native_action_resolution_script(
    target: &BrowserTargetRecord,
    action: &tidebreak_core::BrowserAction,
) -> Result<String, String> {
    native_action_resolution_script_for_phase(target, action, NativeActionDispatchPhase::Initial)
}

fn native_action_resolution_script_for_phase(
    target: &BrowserTargetRecord,
    action: &tidebreak_core::BrowserAction,
    phase: NativeActionDispatchPhase,
) -> Result<String, String> {
    let payload = serde_json::json!({
        "previousSelectedIndex": match phase {
            NativeActionDispatchPhase::SelectFollowUp { previous_selected_index, .. } => Some(previous_selected_index),
            _ => None,
        },
        "focusStage": match phase {
            NativeActionDispatchPhase::FocusVerify => "verify",
            NativeActionDispatchPhase::PressKey | NativeActionDispatchPhase::SelectInitialStep | NativeActionDispatchPhase::SelectFollowUp { .. } => "required",
            _ => "acquire",
        },
        "fillStage": match phase {
            NativeActionDispatchPhase::FillSelectAll => "select_all",
            NativeActionDispatchPhase::FillInsert => "insert",
            NativeActionDispatchPhase::FillVerify => "verify",
            _ => "focus",
        },
        "framePath": target.frame_path,
        "selector": target.selector,
        "marker": target.marker,
        "markerValue": target.marker_value,
        "fingerprint": {
            "tag": target.fingerprint.tag,
            "role": target.fingerprint.role,
            "name": target.fingerprint.name,
            "inputType": target.fingerprint.input_type,
            "href": target.fingerprint.href,
            "sensitive": target.fingerprint.sensitive,
        },
        "action": match action {
            tidebreak_core::BrowserAction::Click => serde_json::json!({ "type": "click" }),
            tidebreak_core::BrowserAction::Focus => serde_json::json!({ "type": "focus" }),
            tidebreak_core::BrowserAction::Hover => serde_json::json!({ "type": "hover" }),
            tidebreak_core::BrowserAction::Fill { value } => {
                serde_json::json!({ "type": "fill", "value": value })
            }
            tidebreak_core::BrowserAction::Select { value } => {
                serde_json::json!({ "type": "select", "value": value })
            }
            tidebreak_core::BrowserAction::Check { checked } => {
                serde_json::json!({ "type": "check", "checked": checked })
            }
            tidebreak_core::BrowserAction::Press { key } => {
                serde_json::json!({ "type": "press", "key": key })
            }
            tidebreak_core::BrowserAction::ScrollIntoView => {
                serde_json::json!({ "type": "scroll_into_view" })
            }
        },
    });
    let payload = serde_json::to_string(&payload).map_err(|error| error.to_string())?;
    Ok(NATIVE_ACTION_RESOLUTION_SCRIPT
        .replace("__TARGET_IDENTITY_STORE__", TARGET_IDENTITY_STORE_SCRIPT)
        .replace("__SENSITIVE_FIELD_POLICY__", SENSITIVE_FIELD_POLICY)
        .replace("__PAYLOAD__", &payload))
}

fn browser_upload_script(
    target: &BrowserTargetRecord,
    file: &BrowserUploadFile,
) -> Result<String, String> {
    let payload = serde_json::json!({
        "framePath": target.frame_path,
        "selector": target.selector,
        "marker": target.marker,
        "markerValue": target.marker_value,
        "fingerprint": {
            "tag": target.fingerprint.tag,
            "role": target.fingerprint.role,
            "name": target.fingerprint.name,
            "inputType": target.fingerprint.input_type,
            "href": target.fingerprint.href,
            "sensitive": target.fingerprint.sensitive,
        },
        "file": {
            "name": file.filename,
            "mediaType": file.media_type,
            "byteLen": file.binding.byte_len,
            "contentBase64": base64::engine::general_purpose::STANDARD.encode(&file.bytes),
        },
    });
    let payload = serde_json::to_string(&payload).map_err(|error| error.to_string())?;
    Ok(BROWSER_UPLOAD_SCRIPT
        .replace("__TARGET_IDENTITY_STORE__", TARGET_IDENTITY_STORE_SCRIPT)
        .replace("__SENSITIVE_FIELD_POLICY__", SENSITIVE_FIELD_POLICY)
        .replace("__PAYLOAD__", &payload))
}

fn wait_text_script(text: &str) -> Result<String, String> {
    let text = serde_json::to_string(text).map_err(|error| error.to_string())?;
    Ok(WAIT_TEXT_SCRIPT
        .replace("__WAIT_TEXT__", &text)
        .replace("__SENSITIVE_FIELD_POLICY__", SENSITIVE_FIELD_POLICY))
}

fn inspect_overlay_script() -> String {
    INSPECT_OVERLAY_SCRIPT.replace("__SENSITIVE_FIELD_POLICY__", SENSITIVE_FIELD_POLICY)
}

fn screenshot_privacy_script(watch_key: &str, finish: bool) -> Result<String, String> {
    let watch_key = serde_json::to_string(watch_key).map_err(|error| error.to_string())?;
    Ok(SCREENSHOT_PRIVACY_SCRIPT
        .replace("__WATCH_KEY__", &watch_key)
        .replace("__FINISH__", if finish { "true" } else { "false" })
        .replace("__SENSITIVE_FIELD_POLICY__", SENSITIVE_FIELD_POLICY))
}

struct NativeUploadAuthorization {
    registry: BrowserRegistry,
    capability_id: Uuid,
    workspace_id: String,
    origin: BrowserOrigin,
    fence: BrowserObservationFence,
    arguments: tidebreak_core::BrowserUploadArgs,
    target: BrowserTargetRecord,
}

impl NativeUploadAuthorization {
    fn authorize(&self) -> Result<(), String> {
        self.registry.authorize_native_action_phase(
            self.capability_id,
            &self.arguments.browser_id,
            &self.workspace_id,
            &self.origin,
            self.fence,
        )?;
        let host = self
            .registry
            .begin_agent_observation(self.capability_id, &self.arguments.browser_id)?;
        if !host
            .agent_access
            .as_ref()
            .is_some_and(|access| access.can_transfer_files)
        {
            return Err("browser origin is not shared for this operation".to_owned());
        }
        let target = self
            .registry
            .semantic_target(
                &self.arguments.browser_id,
                &self.workspace_id,
                &self.arguments.snapshot_id,
                self.arguments.document_epoch,
                &self.arguments.target_ref,
            )
            .map_err(|error| target_error_message(error).to_owned())?;
        if target != self.target || !is_file_input(&target) {
            return Err(target_error_message(BrowserTargetError::StaleTarget).to_owned());
        }
        Ok(())
    }
}

#[cfg(any(target_os = "macos", test))]
struct NativeUploadCallbackState {
    sender: Option<oneshot::Sender<Result<String, String>>>,
    cancelled: bool,
    deadline: tokio::time::Instant,
}

#[cfg(any(target_os = "macos", test))]
struct NativeUploadCancellation(std::sync::Arc<std::sync::Mutex<NativeUploadCallbackState>>);

#[cfg(any(target_os = "macos", test))]
impl Drop for NativeUploadCancellation {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.lock() {
            state.cancelled = true;
            state.sender.take();
        }
    }
}

#[cfg(any(target_os = "macos", test))]
fn submit_native_browser_upload(
    state: &std::sync::Mutex<NativeUploadCallbackState>,
    authorization: &NativeUploadAuthorization,
    submit: impl FnOnce(),
) -> Result<(), String> {
    let state = state
        .lock()
        .map_err(|_| "browser upload callback is unavailable".to_owned())?;
    if state.cancelled || state.sender.as_ref().is_none_or(oneshot::Sender::is_closed) {
        return Err("browser upload was cancelled".to_owned());
    }
    if tokio::time::Instant::now() >= state.deadline {
        return Err("browser JavaScript timed out".to_owned());
    }
    authorization.authorize()?;
    // Keep cancellation serialized with the actual queued script submission.
    submit();
    drop(state);
    Ok(())
}

#[cfg(target_os = "macos")]
async fn evaluate_browser_upload(
    webview: &Webview,
    script: String,
    authorization: NativeUploadAuthorization,
) -> Result<RawBrowserUploadResult, String> {
    use std::sync::{Arc, Mutex};

    use block2::RcBlock;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSError, NSString};

    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(JAVASCRIPT_TIMEOUT_SECONDS);
    let (sender, receiver) = oneshot::channel();
    let state = Arc::new(Mutex::new(NativeUploadCallbackState {
        sender: Some(sender),
        cancelled: false,
        deadline,
    }));
    let _cancellation = NativeUploadCancellation(Arc::clone(&state));
    let callback_state = Arc::clone(&state);
    with_browser_webview(webview, move |view| unsafe {
        let result_state = Arc::clone(&callback_state);
        let handler = RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
            let Some(sender) = result_state
                .lock()
                .ok()
                .and_then(|mut state| state.sender.take())
            else {
                return;
            };
            if !error.is_null() {
                let message = (&*error).localizedDescription().to_string();
                let _ = sender.send(Err(format!("browser JavaScript failed: {message}")));
                return;
            }
            if value.is_null() {
                let _ = sender.send(Err("browser JavaScript returned no value".to_owned()));
                return;
            }
            let value: &NSString = &*value.cast();
            let _ = sender.send(Ok(value.to_string()));
        });
        let result = browser_semantics_content_world().and_then(|content_world| {
            let script = NSString::from_str(&script);
            submit_native_browser_upload(&callback_state, &authorization, || {
                view.evaluateJavaScript_inFrame_inContentWorld_completionHandler(
                    &script,
                    None,
                    &content_world,
                    Some(&handler),
                );
            })
        });
        if let Err(error) = result {
            if let Some(sender) = callback_state
                .lock()
                .ok()
                .and_then(|mut state| state.sender.take())
            {
                let _ = sender.send(Err(error));
            }
        }
    })?;

    let raw = tokio::time::timeout_at(deadline, receiver)
        .await
        .map_err(|_| "browser JavaScript timed out".to_owned())?
        .map_err(|_| "browser JavaScript was interrupted".to_owned())??;
    serde_json::from_str(&raw).map_err(|error| format!("invalid browser response: {error}"))
}

#[cfg(not(target_os = "macos"))]
async fn evaluate_browser_upload(
    _webview: &Webview,
    _script: String,
    authorization: NativeUploadAuthorization,
) -> Result<RawBrowserUploadResult, String> {
    authorization.authorize()?;
    Err("semantic browser control is not available on this platform yet".to_owned())
}

#[cfg(target_os = "macos")]
async fn evaluate_json<T: serde::de::DeserializeOwned>(
    webview: &Webview,
    script: &str,
) -> Result<T, String> {
    use std::sync::Mutex;

    use block2::RcBlock;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSError, NSString};
    use tokio::{sync::oneshot, time::timeout};

    let (sender, receiver) = oneshot::channel();
    let sender = Mutex::new(Some(sender));
    let script = script.to_owned();
    with_browser_webview(webview, move |view| unsafe {
        let content_world = match browser_semantics_content_world() {
            Ok(content_world) => content_world,
            Err(error) => {
                if let Some(sender) = sender.lock().ok().and_then(|mut sender| sender.take()) {
                    let _ = sender.send(Err(error));
                }
                return;
            }
        };
        let handler = RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
            let Some(sender) = sender.lock().ok().and_then(|mut sender| sender.take()) else {
                return;
            };
            if !error.is_null() {
                let message = (&*error).localizedDescription().to_string();
                let _ = sender.send(Err(format!("browser JavaScript failed: {message}")));
                return;
            }
            if value.is_null() {
                let _ = sender.send(Err("browser JavaScript returned no value".to_owned()));
                return;
            }
            let value: &NSString = &*value.cast();
            let _ = sender.send(Ok(value.to_string()));
        });
        let script = NSString::from_str(&script);
        view.evaluateJavaScript_inFrame_inContentWorld_completionHandler(
            &script,
            None,
            &content_world,
            Some(&handler),
        );
    })?;

    let raw = timeout(
        std::time::Duration::from_secs(JAVASCRIPT_TIMEOUT_SECONDS),
        receiver,
    )
    .await
    .map_err(|_| "browser JavaScript timed out".to_owned())?
    .map_err(|_| "browser JavaScript was interrupted".to_owned())??;
    serde_json::from_str(&raw).map_err(|error| format!("invalid browser response: {error}"))
}

#[cfg(not(target_os = "macos"))]
async fn evaluate_json<T: serde::de::DeserializeOwned>(
    _webview: &Webview,
    _script: &str,
) -> Result<T, String> {
    Err("semantic browser control is not available on this platform yet".to_owned())
}

const SENSITIVE_FIELD_POLICY: &str = r##"
  const tidebreakFieldSelector = "input, textarea, [contenteditable]:not([contenteditable='false']), [role='textbox']";
  const tidebreakTextWithoutFieldDescendants = (element, limit = 240) => {
    const parts = [];
    const visit = (node, insideField) => {
      if (!node) return;
      if (node.nodeType === 3) {
        if (!insideField) parts.push(node.nodeValue || "");
        return;
      }
      const nextInsideField = insideField
        || (node !== element && Boolean(node.matches?.(tidebreakFieldSelector)));
      for (const child of Array.from(node.childNodes || [])) {
        visit(child, nextInsideField);
      }
    };
    visit(element, Boolean(element?.matches?.(tidebreakFieldSelector)));
    return clean(parts.join(" "), limit);
  };
  const tidebreakReferencedText = (element) => {
    if (!element || element.matches?.(tidebreakFieldSelector)) return "";
    return tidebreakTextWithoutFieldDescendants(element);
  };
  const tidebreakAriaName = (element, doc) => {
    const labelledBy = clean(element.getAttribute("aria-labelledby"), 200);
    if (labelledBy) {
      const root = element.getRootNode?.();
      const value = labelledBy.split(/\s+/)
        .map((id) => clean(
          tidebreakReferencedText(
            root?.getElementById?.(id) || doc.getElementById(id),
          ),
        ))
        .filter(Boolean)
        .join(" ");
      if (value) return clean(value);
    }
    return clean(element.getAttribute("aria-label"));
  };
  const tidebreakAccessibleName = (element, doc) => {
    const direct = tidebreakAriaName(element, doc)
      || clean(element.labels && Array.from(element.labels).map(tidebreakReferencedText).join(" "))
      || clean(element.getAttribute("alt"))
      || clean(element.getAttribute("title"))
      || clean(element.getAttribute("placeholder"));
    if (direct) return direct;
    return tidebreakTextWithoutFieldDescendants(element);
  };
  const tidebreakDescribedText = (element, doc) => clean(
    clean(element.getAttribute("aria-describedby"), 200)
      .split(/\s+/)
      .map((id) => clean(
        tidebreakReferencedText(
          element.getRootNode?.()?.getElementById?.(id) || doc.getElementById(id),
        ),
      ))
      .filter(Boolean)
      .join(" "),
    240,
  );
  const tidebreakSensitiveSignal = (value) => clean(value, 500)
    .normalize("NFKC")
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1 $2")
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, " ")
    .trim();
  const tidebreakIsSensitiveField = (element, doc) => {
    const tag = element.localName || "";
    const role = clean(element.getAttribute("role"), 60).toLowerCase();
    const editable = tag === "input"
      || tag === "textarea"
      || element.isContentEditable
      || role === "textbox";
    if (!editable) return false;

    const type = clean(element.getAttribute("type") || element.type, 40).toLowerCase();
    const autocomplete = clean(element.getAttribute("autocomplete"), 160).toLowerCase();
    if (type === "password" || type === "file") return true;
    if (["one-time-code", "current-password", "new-password", "cc-csc"]
      .some((token) => autocomplete.split(/\s+/).includes(token))) return true;

    const form = element.form;
    const group = element.closest("[role='group']");
    const accessibleName = tidebreakAccessibleName(element, doc);
    const signal = tidebreakSensitiveSignal([
      element.getAttribute("name"),
      element.id,
      accessibleName,
      element.getAttribute("placeholder"),
      element.getAttribute("aria-label"),
      element.getAttribute("title"),
      tidebreakDescribedText(element, doc),
      element.closest("fieldset")?.querySelector("legend")?.textContent,
      group && tidebreakAriaName(group, doc),
      form?.getAttribute("name"),
      form?.id,
      form && tidebreakAriaName(form, doc),
      form?.querySelector("legend, h1, h2, h3")?.textContent,
    ].filter(Boolean).join(" "));
    const benignNumeric = /\b(quantity|qty|count|amount|price|year|age|zip|postal|search|phone|mobile|page|size|score|rating|percent|percentage|width|height|distance|duration)\b/.test(signal);
    const explicitSensitive = /\b(one time|otp|totp|verification|verify|auth|authentication|authenticator|security|recovery|backup|two factor|2fa|mfa|passcode|password|passphrase|secret|pin|cvv|cvc|cid)\b/.test(signal);
    const codeOrToken = /\b(code|token)\b/.test(signal);
    if (explicitSensitive || (!benignNumeric && codeOrToken)) return true;

    const inputMode = clean(element.getAttribute("inputmode") || element.inputMode, 40).toLowerCase();
    const pattern = clean(element.getAttribute("pattern"), 120);
    const maxLength = Number(element.getAttribute("maxlength") || element.maxLength || 0);
    const size = Number(element.getAttribute("size") || element.size || 0);
    const numeric = ["numeric", "decimal", "tel"].includes(inputMode)
      || ["number", "tel"].includes(type)
      || /\\d|\[0-9\]|digit/i.test(pattern);
    const shortConstraint = (maxLength >= 3 && maxLength <= 12)
      || (size >= 3 && size <= 12);
    const patternBounds = pattern.match(/\{\s*(\d{1,2})\s*(?:,\s*(\d{1,2})?\s*)?\}/);
    const patternMin = Number(patternBounds?.[1]);
    const patternMax = patternBounds?.[0].includes(",")
      ? Number(patternBounds?.[2] || Number.POSITIVE_INFINITY)
      : patternMin;
    const shortDigitPattern = /\\d|\[0-9\]|digit/i.test(pattern)
      && patternMin >= 3
      && patternMax <= 12;
    const looksLikeShortDigits = (value) => {
      const example = clean(value, 80).normalize("NFKC").trim();
      return /^(?:\d[\s\-–—]*){3,12}$/.test(example);
    };
    const digitExample = [
      element.getAttribute("placeholder"),
      element.getAttribute("aria-label"),
      accessibleName,
    ].some(looksLikeShortDigits);
    if (numeric && !benignNumeric && (element.isContentEditable || role === "textbox")) {
      return true;
    }
    if (numeric && !benignNumeric && (shortConstraint || shortDigitPattern || digitExample)) {
      return true;
    }

    if (numeric && !benignNumeric && maxLength === 1) {
      const container = element.closest("fieldset, [role='group'], form, div");
      const peers = container
        ? Array.from(container.querySelectorAll("input")).filter((candidate) => {
          const candidateMode = clean(candidate.getAttribute("inputmode") || candidate.inputMode, 40).toLowerCase();
          const candidateType = clean(candidate.getAttribute("type") || candidate.type, 40).toLowerCase();
          const candidateLength = Number(candidate.getAttribute("maxlength") || candidate.maxLength || 0);
          return candidateLength === 1
            && (["numeric", "decimal", "tel"].includes(candidateMode)
              || ["number", "tel"].includes(candidateType));
        })
        : [];
      if (peers.length >= 4 && peers.length <= 12) return true;
    }
    return false;
  };
"##;

const TARGET_IDENTITY_STORE_SCRIPT: &str = r#"
  const tidebreakTargetIdentityStore = (() => {
    const key = Symbol.for("io.brightwave.tidebreak.browser.target-identities");
    const existing = globalThis[key];
    if (existing instanceof WeakMap) return existing;
    const store = new WeakMap();
    Object.defineProperty(globalThis, key, {
      value: store,
      configurable: false,
      enumerable: false,
      writable: false,
    });
    return store;
  })();
"#;

const WAIT_TEXT_SCRIPT: &str = r#"
(() => {
  const NEEDLE = __WAIT_TEXT__;
  const TEXT_LIMIT = 1_000_000;
  const clean = (value, limit = 240) => String(value || "")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, limit);
  __SENSITIVE_FIELD_POLICY__

  const isVisible = (element, win) => {
    const style = win.getComputedStyle(element);
    const rect = element.getBoundingClientRect();
    return style.display !== "none"
      && style.visibility !== "hidden"
      && Number(style.opacity || 1) !== 0
      && rect.width > 0
      && rect.height > 0;
  };
  const ordinaryFieldText = (element) => {
    if ("value" in element) return clean(element.value, TEXT_LIMIT);
    return clean(element.textContent, TEXT_LIMIT);
  };
  const collectRoot = (root, doc, win, seen) => {
    if (seen.has(root)) return { parts: [], uninspectableRegions: 0 };
    seen.add(root);
    const parts = [];
    let uninspectableRegions = 0;
    if (root.body || root.nodeType === 11) {
      const text = tidebreakTextWithoutFieldDescendants(root.body || root, TEXT_LIMIT);
      if (text) parts.push(text);
    }
    for (const field of root.querySelectorAll(tidebreakFieldSelector)) {
      if (!isVisible(field, win) || tidebreakIsSensitiveField(field, doc)) continue;
      const text = ordinaryFieldText(field);
      if (text) parts.push(text);
    }
    for (const host of root.querySelectorAll("*")) {
      if (!host.shadowRoot) continue;
      const child = collectRoot(host.shadowRoot, doc, win, seen);
      parts.push(...child.parts);
      uninspectableRegions += child.uninspectableRegions;
    }
    for (const frame of root.querySelectorAll("iframe, frame, object, embed")) {
      if (!isVisible(frame, win)) continue;
      if (frame.localName === "embed") {
        uninspectableRegions += 1;
        continue;
      }
      try {
        const childDoc = frame.contentDocument;
        const childWin = frame.contentWindow || childDoc?.defaultView;
        if (!childDoc || !childWin) throw new Error("nested document unavailable");
        const child = collectRoot(childDoc, childDoc, childWin, seen);
        parts.push(...child.parts);
        uninspectableRegions += child.uninspectableRegions;
      } catch (_) {
        uninspectableRegions += 1;
      }
    }
    return { parts, uninspectableRegions };
  };

  const projection = collectRoot(document, document, window, new Set());
  const text = clean(projection.parts.join(" "), TEXT_LIMIT);
  return JSON.stringify({
    contains: text.includes(NEEDLE),
    uninspectableRegions: projection.uninspectableRegions,
  });
})()
"#;

const SNAPSHOT_SCRIPT: &str = r#"
(() => {
  const MAX_NODES = __MAX_NODES__;
  const MARKER = "__MARKER__";
  const TEXT_LIMIT = 240;
  const nodes = [];
  const frames = [];
  let truncated = false;
  let nextTargetRef = 1;
  __TARGET_IDENTITY_STORE__

  const INTERACTIVE_SELECTOR = [
    "a[href]", "button", "input:not([type='hidden'])", "textarea", "select",
    "[contenteditable]:not([contenteditable='false'])", "[role='textbox']",
    "[role='button']", "[role='link']",
    "[role='checkbox']", "[role='radio']", "[role='tab']",
    "[tabindex]:not([tabindex='-1'])"
  ].join(",");
  const CONTENT_SELECTOR = [
    "h1", "h2", "h3", "h4", "h5", "h6", "p", "li", "dt", "dd",
    "pre", "blockquote", "figcaption", "img[alt]", "[role='heading']",
    "[role='status']", "[role='alert']"
  ].join(",");

  const clean = (value, limit = TEXT_LIMIT) => String(value || "")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, limit);
  __SENSITIVE_FIELD_POLICY__
  const escapeCss = (value) => globalThis.CSS?.escape
    ? globalThis.CSS.escape(value)
    : String(value).replace(/[^a-zA-Z0-9_-]/g, (character) => `\\${character}`);
  const selectorFor = (element, doc) => {
    if (element.id) {
      const candidate = `#${escapeCss(element.id)}`;
      try {
        if (doc.querySelectorAll(candidate).length === 1) return candidate;
      } catch (_) {}
    }
    const parts = [];
    let current = element;
    while (current && current.nodeType === Node.ELEMENT_NODE) {
      const tag = current.localName;
      if (!tag) break;
      let index = 1;
      let sibling = current.previousElementSibling;
      while (sibling) {
        if (sibling.localName === tag) index += 1;
        sibling = sibling.previousElementSibling;
      }
      parts.unshift(`${tag}:nth-of-type(${index})`);
      if (current === doc.documentElement) break;
      current = current.parentElement;
    }
    return parts.join(" > ");
  };
  const inferredRole = (element) => {
    const explicit = clean(element.getAttribute("role"), 60);
    if (explicit) return explicit;
    const tag = element.localName;
    const type = clean(element.getAttribute("type"), 40).toLowerCase();
    if (tag === "a" && element.hasAttribute("href")) return "link";
    if (tag === "button") return "button";
    if (tag === "textarea") return "textbox";
    if (tag === "select") return "combobox";
    if (/^h[1-6]$/.test(tag)) return "heading";
    if (tag === "p") return "paragraph";
    if (tag === "li") return "listitem";
    if (tag === "img") return "img";
    if (tag === "pre") return "code";
    if (tag === "blockquote") return "blockquote";
    if (tag === "input") {
      if (type === "checkbox") return "checkbox";
      if (type === "radio") return "radio";
      if (["button", "submit", "reset", "image"].includes(type)) return "button";
      return "textbox";
    }
    if (element.isContentEditable) return "textbox";
    return tag || "element";
  };
  const isConsequential = (element) => {
    const tag = element.localName;
    const type = clean(element.getAttribute("type") || element.type, 40).toLowerCase();
    return type === "submit"
      || type === "file"
      || (tag === "button" && Boolean(element.form) && (!type || type === "submit"));
  };
  const isVisible = (element, win) => {
    const style = win.getComputedStyle(element);
    const rect = element.getBoundingClientRect();
    return style.display !== "none"
      && style.visibility !== "hidden"
      && Number(style.opacity || 1) !== 0
      && rect.width > 0
      && rect.height > 0;
  };
  const actionsFor = (element, role, sensitive) => {
    const nativePopupSelect = element.localName === "select" && !element.multiple && element.size <= 1;
    if (sensitive || nativePopupSelect) return ["human_takeover"];
    const actions = ["focus", "hover", "scroll_into_view"];
    if (["button", "link", "checkbox", "radio", "tab"].includes(role)) actions.unshift("click");
    if (role === "textbox") actions.unshift("fill");
    if (role === "combobox") actions.unshift("select");
    if (role === "checkbox" || role === "radio") actions.unshift("check");
    actions.push("press");
    return Array.from(new Set(actions));
  };
  const contentText = (element) => element.localName === "img"
    ? clean(element.getAttribute("alt"))
    : tidebreakTextWithoutFieldDescendants(element);

  const visit = (doc, win, framePath, frameName, offsetX, offsetY) => {
    const candidates = doc.querySelectorAll(`${INTERACTIVE_SELECTOR},${CONTENT_SELECTOR}`);
    for (const element of candidates) {
      if (nodes.length >= MAX_NODES) {
        truncated = true;
        break;
      }
      if (!isVisible(element, win)) continue;
      const interactive = element.matches(INTERACTIVE_SELECTOR);
      if (!interactive && element.closest(INTERACTIVE_SELECTOR)) continue;
      const rect = element.getBoundingClientRect();
      const role = inferredRole(element);
      const sensitive = interactive && tidebreakIsSensitiveField(element, doc);
      const text = sensitive ? "" : contentText(element);
      if (!interactive && !text) continue;
      const consequential = interactive && isConsequential(element);
      const targetRef = interactive ? `@e${nextTargetRef++}` : null;
      if (interactive) {
        tidebreakTargetIdentityStore.set(element, {
          snapshotMarker: MARKER,
          targetRef,
        });
      }
      const inputType = element.localName === "input"
        ? clean(element.getAttribute("type") || "text", 40).toLowerCase()
        : null;
      const value = !interactive || sensitive
        ? null
        : ("value" in element ? clean(element.value) : null);
      nodes.push({
        kind: interactive ? "interactive" : "content",
        ref: targetRef,
        tag: element.localName || "element",
        role,
        name: interactive
          ? (sensitive ? "Sensitive field" : tidebreakAccessibleName(element, doc))
          : text,
        frame: frameName,
        text: sensitive ? null : (text || null),
        value: value || null,
        href: interactive && !sensitive && element.href ? clean(element.href, 2048) : null,
        inputType,
        disabled: interactive && Boolean(element.disabled || element.getAttribute("aria-disabled") === "true"),
        checked: interactive && "checked" in element ? Boolean(element.checked) : null,
        sensitive,
        consequential,
        actions: interactive ? actionsFor(element, role, sensitive) : [],
        bounds: {
          x: Number((offsetX + rect.x).toFixed(2)),
          y: Number((offsetY + rect.y).toFixed(2)),
          width: Number(rect.width.toFixed(2)),
          height: Number(rect.height.toFixed(2)),
        },
        selector: interactive ? selectorFor(element, doc) : null,
        framePath,
      });
    }

    for (const [index, frame] of Array.from(doc.querySelectorAll("iframe")).entries()) {
      const selector = selectorFor(frame, doc);
      const name = `${frameName}/frame-${index + 1}`;
      const declaredUrl = clean(frame.getAttribute("src") || "about:blank", 2048);
      try {
        const childDoc = frame.contentDocument;
        const childWin = frame.contentWindow;
        if (!childDoc || !childWin) throw new Error("frame unavailable");
        frames.push({ name, url: clean(childWin.location.href, 2048), status: "same_origin" });
        if (nodes.length < MAX_NODES) {
          const rect = frame.getBoundingClientRect();
          visit(
            childDoc,
            childWin,
            [...framePath, selector],
            name,
            offsetX + rect.x,
            offsetY + rect.y,
          );
        }
      } catch (_) {
        frames.push({ name, url: declaredUrl, status: "unsupported_frame" });
      }
    }
  };

  visit(document, window, [], "top", 0, 0);
  return JSON.stringify({
    url: String(location.href),
    title: clean(document.title, 160),
    viewport: {
      width: window.innerWidth,
      height: window.innerHeight,
      scrollX: window.scrollX,
      scrollY: window.scrollY,
    },
    nodes,
    frames,
    truncated,
  });
})()
"#;

const INSPECT_OVERLAY_SCRIPT: &str = r##"
(() => {
  const STATE_KEY = "__tidebreak_inspect_state__";
  const active = window[STATE_KEY];
  if (active && !active.cancelled) return "already_injected";

  const INTERACTIVE_SELECTOR = "a[href], button, input:not([type='hidden']), textarea, select, [contenteditable]:not([contenteditable='false']), [role='textbox'], [role='button'], [role='link'], [role='checkbox'], [role='radio'], [role='tab'], [tabindex]:not([tabindex='-1'])";
  const FIELD_SELECTOR = "input, textarea, [contenteditable]:not([contenteditable='false']), [role='textbox']";
  const clean = (value, limit = 240) => String(value || "")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, limit);
  __SENSITIVE_FIELD_POLICY__

  const MAX_OVERLAY_ENTRIES = 500;
  const MAX_OBSERVED_ROOTS = 64;
  const MAX_SCANNED_NODES = 5000;
  const OBSERVED_ATTRIBUTES = [
    "aria-describedby", "aria-hidden", "aria-label", "aria-labelledby", "autocomplete",
    "class", "contenteditable", "disabled", "hidden", "href", "id", "inputmode",
    "name", "open", "placeholder", "readonly", "role", "style", "tabindex", "type",
  ];
  const state = {
    cancelled: false,
    frame: null,
    host: null,
    listeners: [],
    observers: [],
    schedule: null,
  };
  window[STATE_KEY] = state;

  const isVisible = (element, win) => {
    const style = win.getComputedStyle(element);
    const rect = element.getBoundingClientRect();
    return style.display !== "none"
      && style.visibility !== "hidden"
      && Number(style.opacity || 1) !== 0
      && rect.width > 0
      && rect.height > 0
      && rect.right > 0
      && rect.bottom > 0
      && rect.left < win.innerWidth
      && rect.top < win.innerHeight;
  };
  const absoluteRect = (rect, offsetX, offsetY) => ({
    left: offsetX + rect.left,
    top: offsetY + rect.top,
    width: rect.width,
    height: rect.height,
  });
  const ordinaryLabel = (element) => (element.localName || "element")
    + (element.id ? "#" + element.id : "")
    + (element.className && typeof element.className === "string"
      ? "." + element.className.split(" ").filter(Boolean).slice(0, 2).join(".")
      : "");
  const addMask = (entries, rect, label = "Sensitive region · human takeover") => {
    if (entries.length >= MAX_OVERLAY_ENTRIES) return;
    entries.push({ rect, sensitive: true, label });
  };

  const scanRoot = (root, doc, win, offsetX, offsetY, entries, roots, budget) => {
    if (
      roots.length >= MAX_OBSERVED_ROOTS
      || entries.length >= MAX_OVERLAY_ENTRIES
      || budget.nodes <= 0
    ) {
      return true;
    }
    roots.push({ root, win });
    let containsSensitive = false;
    const treeDocument = root.nodeType === 9 ? root : root.ownerDocument;
    if (!treeDocument) return true;
    const walker = treeDocument.createTreeWalker(root, 1);
    while (walker.nextNode()) {
      if (budget.nodes <= 0) return true;
      budget.nodes -= 1;
      const element = walker.currentNode;
      if (element.matches?.(FIELD_SELECTOR) && tidebreakIsSensitiveField(element, doc)) {
        containsSensitive = true;
      }
      if (element.matches?.(INTERACTIVE_SELECTOR)) {
        if (entries.length >= MAX_OVERLAY_ENTRIES) return true;
        const sensitive = tidebreakIsSensitiveField(element, doc);
        if (isVisible(element, win)) {
          entries.push({
            rect: absoluteRect(element.getBoundingClientRect(), offsetX, offsetY),
            sensitive,
            label: sensitive ? "Sensitive field · human takeover" : ordinaryLabel(element),
          });
        }
      }

      if (element.shadowRoot) {
        const entryStart = entries.length;
        const shadowSensitive = scanRoot(
          element.shadowRoot,
          doc,
          win,
          offsetX,
          offsetY,
          entries,
          roots,
          budget,
        );
        if (shadowSensitive) {
          entries.splice(entryStart);
          addMask(entries, absoluteRect(element.getBoundingClientRect(), offsetX, offsetY));
          containsSensitive = true;
        }
      } else if (element.localName?.includes("-")) {
        if (isVisible(element, win)) {
          addMask(entries, absoluteRect(element.getBoundingClientRect(), offsetX, offsetY));
        }
        containsSensitive = true;
      }
      if (element.localName === "iframe") {
        const frameRect = absoluteRect(element.getBoundingClientRect(), offsetX, offsetY);
        try {
          const childDoc = element.contentDocument;
          const childWin = element.contentWindow;
          if (!childDoc || !childWin) throw new Error("frame unavailable");
          const entryStart = entries.length;
          const childSensitive = scanRoot(
            childDoc,
            childDoc,
            childWin,
            frameRect.left,
            frameRect.top,
            entries,
            roots,
            budget,
          );
          if (childSensitive) {
            entries.splice(entryStart);
            addMask(entries, frameRect);
            containsSensitive = true;
          }
        } catch (_) {
          if (isVisible(element, win)) {
            addMask(entries, frameRect, "Uninspectable frame · human takeover");
          }
          containsSensitive = true;
        }
      }
    }
    return containsSensitive;
  };

  const setImportant = (style, name, value) => style.setProperty(name, value, "important");
  const buildMarker = (entry, shadow) => {
    const marker = document.createElement("div");
    setImportant(marker.style, "all", "initial");
    setImportant(marker.style, "position", "fixed");
    setImportant(marker.style, "left", entry.rect.left + "px");
    setImportant(marker.style, "top", entry.rect.top + "px");
    setImportant(marker.style, "width", entry.rect.width + "px");
    setImportant(marker.style, "height", entry.rect.height + "px");
    setImportant(marker.style, "box-sizing", "border-box");
    setImportant(marker.style, "pointer-events", "none");
    setImportant(marker.style, "border-radius", "3px");
    setImportant(marker.style, "border", entry.sensitive
      ? "1.5px solid rgb(157, 64, 40)"
      : "1.5px solid rgba(20, 130, 240, 0.72)");
    setImportant(marker.style, "background", entry.sensitive
      ? "rgb(247, 240, 232)"
      : "rgba(20, 130, 240, 0.09)");

    const label = document.createElement("span");
    label.textContent = entry.label;
    setImportant(label.style, "all", "initial");
    setImportant(label.style, "position", "absolute");
    setImportant(label.style, "left", "0");
    setImportant(label.style, "top", "0");
    setImportant(label.style, "box-sizing", "border-box");
    setImportant(label.style, "max-width", Math.max(1, entry.rect.width) + "px");
    setImportant(label.style, "overflow", "hidden");
    setImportant(label.style, "text-overflow", "ellipsis");
    setImportant(label.style, "white-space", "nowrap");
    setImportant(label.style, "padding", "1.5px 4px");
    setImportant(label.style, "font-family", "-apple-system, BlinkMacSystemFont, Segoe UI, sans-serif");
    setImportant(label.style, "font-size", "9px");
    setImportant(label.style, "font-weight", "600");
    setImportant(label.style, "line-height", "1");
    setImportant(label.style, "color", entry.sensitive ? "rgb(126, 48, 29)" : "rgb(20, 130, 240)");
    setImportant(label.style, "background", entry.sensitive ? "rgb(255, 248, 240)" : "rgba(255, 255, 255, 0.92)");
    marker.appendChild(label);
    shadow.appendChild(marker);
  };

  const schedule = () => {
    if (state.cancelled || state.frame !== null) return;
    state.frame = window.requestAnimationFrame(() => {
      state.frame = null;
      render();
    });
  };
  state.schedule = schedule;
  const render = () => {
    if (state.cancelled) return;
    const entries = [];
    const roots = [];
    const budget = { nodes: MAX_SCANNED_NODES };
    const takeoverRequired = scanRoot(
      document,
      document,
      window,
      0,
      0,
      entries,
      roots,
      budget,
    );
    if (takeoverRequired) {
      entries.splice(0, entries.length, {
        rect: { left: 0, top: 0, width: window.innerWidth, height: window.innerHeight },
        sensitive: true,
        label: "Sensitive content · human takeover",
      });
    }

    const host = document.createElement("div");
    host.setAttribute("data-tidebreak-inspect", "true");
    setImportant(host.style, "all", "initial");
    setImportant(host.style, "position", "fixed");
    setImportant(host.style, "inset", "0");
    setImportant(host.style, "display", "block");
    setImportant(host.style, "pointer-events", "none");
    setImportant(host.style, "z-index", "2147483647");
    const shadow = host.attachShadow({ mode: "closed" });
    for (const entry of entries) buildMarker(entry, shadow);
    host.addEventListener("toggle", (event) => {
      if (event.newState === "closed" && state.host === host) schedule();
    });

    for (const observer of state.observers) observer.disconnect();
    state.observers = [];
    for (const [target, type] of state.listeners) {
      target.removeEventListener(type, schedule, true);
    }
    state.listeners = [];
    if (state.host?.isConnected) state.host.replaceWith(host);
    else document.documentElement.appendChild(host);
    if (typeof host.showPopover === "function") {
      host.setAttribute("popover", "manual");
      try { host.showPopover(); } catch (_) {}
    }
    state.host = host;

    const listen = (target, type) => {
      if (state.listeners.some(([activeTarget, activeType]) => (
        activeTarget === target && activeType === type
      ))) return;
      target.addEventListener(type, schedule, true);
      state.listeners.push([target, type]);
    };
    for (const observed of roots) {
      const observer = new observed.win.MutationObserver(() => schedule());
      observer.observe(observed.root, {
        subtree: true,
        childList: true,
        characterData: true,
        attributes: true,
        attributeFilter: OBSERVED_ATTRIBUTES,
      });
      state.observers.push(observer);
      listen(observed.win, "scroll");
      listen(observed.win, "resize");
      const observedDocument = observed.root.nodeType === 9
        ? observed.root
        : observed.root.ownerDocument;
      if (observedDocument) {
        listen(observedDocument, "fullscreenchange");
      }
    }
  };

  render();
  return "injected";
})()
"##;

const REMOVE_INSPECT_OVERLAY_SCRIPT: &str = r#"
(() => {
  const state = window.__tidebreak_inspect_state__;
  if (state) {
    state.cancelled = true;
    if (state.frame !== null) window.cancelAnimationFrame(state.frame);
    for (const observer of state.observers || []) observer.disconnect();
    for (const [target, type] of state.listeners || []) {
      target.removeEventListener(type, state.schedule, true);
    }
    state.host?.remove();
    delete window.__tidebreak_inspect_state__;
  }
  document.getElementById("__tidebreak_inspect_overlay__")?.remove();
  document.getElementById("__tidebreak_inspect_style__")?.remove();
  delete window.__tidebreak_highlight_inspect__;
  return "removed";
})()
"#;

const SCREENSHOT_PRIVACY_SCRIPT: &str = r#"
(() => {
  const WATCH_KEY = __WATCH_KEY__;
  const FINISH = __FINISH__;
  const clean = (value, limit = 240) => String(value || "")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, limit);
  __SENSITIVE_FIELD_POLICY__

  const hashText = (seed, value) => {
    let hash = seed >>> 0;
    for (const character of String(value || "")) {
      hash ^= character.codePointAt(0);
      hash = Math.imul(hash, 16777619) >>> 0;
    }
    return hash;
  };

  const scanRoot = (root, doc, win, observers, state) => {
    if (!FINISH) {
      const observer = new win.MutationObserver(() => { state.changed = true; });
      observer.observe(root, {
        subtree: true,
        childList: true,
        characterData: true,
        attributes: true,
      });
      observers.push(observer);
    }

    let sensitiveFields = 0;
    let uninspectableRegions = 0;
    let signature = 2166136261;
    for (const element of root.querySelectorAll("input, textarea, [contenteditable]:not([contenteditable='false']), [role='textbox']")) {
      signature = hashText(signature, element.localName);
      signature = hashText(signature, element.getAttribute("type"));
      signature = hashText(signature, element.getAttribute("name"));
      signature = hashText(signature, element.id);
      signature = hashText(signature, "value" in element ? element.value : element.textContent);
      if (tidebreakIsSensitiveField(element, doc)) sensitiveFields += 1;
    }
    for (const element of root.querySelectorAll("*")) {
      if (element.shadowRoot) {
        const child = scanRoot(element.shadowRoot, doc, win, observers, state);
        sensitiveFields += child.sensitiveFields;
        uninspectableRegions += child.uninspectableRegions;
        signature = hashText(signature, child.signature);
      } else if (element.localName?.includes("-")) {
        uninspectableRegions += 1;
      }
    }
    for (const frame of root.querySelectorAll("iframe")) {
      if (!FINISH) {
        const markFrameChanged = () => { state.changed = true; };
        frame.addEventListener("load", markFrameChanged, true);
        state.listeners.push([frame, "load", markFrameChanged]);
      }
      try {
        const childDoc = frame.contentDocument;
        const childWin = frame.contentWindow;
        if (!childDoc || !childWin) throw new Error("frame unavailable");
        if (!FINISH) {
          const markFrameChanged = () => { state.changed = true; };
          childWin.addEventListener("pagehide", markFrameChanged, true);
          state.listeners.push([childWin, "pagehide", markFrameChanged]);
        }
        const child = scanRoot(childDoc, childDoc, childWin, observers, state);
        sensitiveFields += child.sensitiveFields;
        uninspectableRegions += child.uninspectableRegions;
        signature = hashText(signature, child.signature);
      } catch (_) {
        uninspectableRegions += 1;
      }
    }
    return { sensitiveFields, uninspectableRegions, signature };
  };

  let state = window[WATCH_KEY];
  if (!FINISH) {
    if (state) {
      for (const observer of state.observers || []) observer.disconnect();
    }
    state = { changed: false, observers: [], listeners: [], signature: null };
    window[WATCH_KEY] = state;
  } else if (!state) {
    return JSON.stringify({ sensitiveFields: 0, uninspectableRegions: 0, changed: true });
  }

  const result = scanRoot(document, document, window, state.observers, state);
  if (!FINISH) {
    state.signature = result.signature;
  } else if (state.signature !== result.signature) {
    state.changed = true;
  }
  if (FINISH) {
    for (const observer of state.observers) observer.disconnect();
    for (const [target, event, listener] of state.listeners || []) {
      target.removeEventListener(event, listener, true);
    }
    delete window[WATCH_KEY];
  }
  return JSON.stringify({
    sensitiveFields: result.sensitiveFields,
    uninspectableRegions: result.uninspectableRegions,
    changed: Boolean(state.changed),
  });
})()
"#;

const NATIVE_ACTION_RESOLUTION_SCRIPT: &str = r#"
(() => {
  const payload = __PAYLOAD__;
  __TARGET_IDENTITY_STORE__
  const clean = (value, limit = 240) => String(value || "")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, limit);
  __SENSITIVE_FIELD_POLICY__
  const roleFor = (element) => {
    const explicit = clean(element.getAttribute("role"), 60);
    if (explicit) return explicit;
    const tag = element.localName;
    const type = clean(element.getAttribute("type"), 40).toLowerCase();
    if (tag === "a" && element.hasAttribute("href")) return "link";
    if (tag === "button") return "button";
    if (tag === "textarea") return "textbox";
    if (tag === "select") return "combobox";
    if (tag === "input") {
      if (type === "checkbox") return "checkbox";
      if (type === "radio") return "radio";
      if (["button", "submit", "reset", "image"].includes(type)) return "button";
      return "textbox";
    }
    if (element.isContentEditable) return "textbox";
    return tag || "element";
  };
  const result = (status, message, extra = {}) => JSON.stringify({
    status,
    message,
    url: String(location.href),
    title: clean(document.title, 160),
    ...extra,
  });
  const box = (left, top, right, bottom) => ({ left, top, right, bottom });
  const intersect = (first, second) => {
    const next = box(
      Math.max(first.left, second.left),
      Math.max(first.top, second.top),
      Math.min(first.right, second.right),
      Math.min(first.bottom, second.bottom),
    );
    return next.right - next.left >= 1 && next.bottom - next.top >= 1 ? next : null;
  };
  const translate = (value, x, y) => box(
    value.left + x,
    value.top + y,
    value.right + x,
    value.bottom + y,
  );
  const viewportRect = (view) => box(0, 0, view.innerWidth, view.innerHeight);
  const clientRect = (element) => {
    const bounds = element.getBoundingClientRect();
    const left = bounds.left + Number(element.clientLeft || 0);
    const top = bounds.top + Number(element.clientTop || 0);
    const width = Number(element.clientWidth || bounds.width || 0);
    const height = Number(element.clientHeight || bounds.height || 0);
    return box(left, top, left + width, top + height);
  };
  const clipsAxis = (value) => ["auto", "scroll", "overlay", "hidden", "clip"].includes(value);
  const scrollsAxis = (value) => ["auto", "scroll", "overlay"].includes(value);
  const clippedRect = (element, view, initial) => {
    let visible = intersect(initial, viewportRect(view));
    for (let ancestor = element.parentElement; visible && ancestor; ancestor = ancestor.parentElement) {
      const style = view.getComputedStyle(ancestor);
      const clip = clientRect(ancestor);
      visible = box(
        clipsAxis(style.overflowX) ? Math.max(visible.left, clip.left) : visible.left,
        clipsAxis(style.overflowY) ? Math.max(visible.top, clip.top) : visible.top,
        clipsAxis(style.overflowX) ? Math.min(visible.right, clip.right) : visible.right,
        clipsAxis(style.overflowY) ? Math.min(visible.bottom, clip.bottom) : visible.bottom,
      );
      if (visible.right - visible.left < 1 || visible.bottom - visible.top < 1) {
        visible = null;
      }
    }
    return visible;
  };
  const clampScrollDelta = (desired, current, maximum) => Math.max(
    -current,
    Math.min(maximum - current, desired),
  );
  const eventPoint = (container, visible, doc) => {
    const width = visible.right - visible.left;
    const height = visible.bottom - visible.top;
    const candidates = [
      [0.5, 0.5],
      [0.25, 0.25],
      [0.75, 0.25],
      [0.25, 0.75],
      [0.75, 0.75],
    ];
    for (const [xRatio, yRatio] of candidates) {
      const x = visible.left + width * xRatio;
      const y = visible.top + height * yRatio;
      const hit = doc.elementFromPoint(x, y);
      if (hit && (!container || hit === container || container.contains(hit))) {
        return { x, y };
      }
    }
    return null;
  };

  let doc = document;
  let offsetX = 0;
  let offsetY = 0;
  const frameChain = [];
  for (const selector of payload.framePath) {
    const frame = doc.querySelector(selector);
    if (!frame) return result("stale_target", "The target frame changed.");
    const frameRect = clientRect(frame);
    frameChain.push({ doc, frame, offsetX, offsetY });
    offsetX += frameRect.left;
    offsetY += frameRect.top;
    try {
      doc = frame.contentDocument;
    } catch (_) {
      return result("unsupported_frame", "The target is inside a cross-origin frame.");
    }
    if (!doc) return result("stale_target", "The target frame is unavailable.");
  }

  const element = doc.querySelector(payload.selector);
  if (!element || !element.isConnected) {
    return result("stale_target", "The target no longer exists.");
  }
  const targetIdentity = tidebreakTargetIdentityStore.get(element);
  if (
    !targetIdentity
    || targetIdentity.snapshotMarker !== payload.marker
    || targetIdentity.targetRef !== payload.markerValue
  ) {
    return result("stale_target", "The target element was replaced.");
  }
  const isSensitive = tidebreakIsSensitiveField(element, doc);
  const fresh = {
    tag: element.localName || "element",
    role: roleFor(element),
    name: isSensitive ? "Sensitive field" : tidebreakAccessibleName(element, doc),
    inputType: element.localName === "input"
      ? clean(element.getAttribute("type") || "text", 40).toLowerCase()
      : null,
    href: element.href ? clean(element.href, 2048) : null,
    sensitive: isSensitive,
  };
  if (isSensitive) {
    return result(
      "human_takeover_required",
      "Password, file, and verification-code fields require human takeover.",
    );
  }
  if (
    fresh.tag !== payload.fingerprint.tag
    || fresh.role !== payload.fingerprint.role
    || fresh.name !== payload.fingerprint.name
    || fresh.inputType !== payload.fingerprint.inputType
    || fresh.href !== payload.fingerprint.href
    || fresh.sensitive !== payload.fingerprint.sensitive
  ) {
    return result("stale_target", "The target's identifying content changed.");
  }
  if (element.disabled || element.getAttribute("aria-disabled") === "true") {
    return result("invalid_value", "The target is disabled.");
  }

  const view = doc.defaultView;
  if (!view) return result("stale_target", "The target document is unavailable.");
  const style = view.getComputedStyle(element);
  const rect = element.getBoundingClientRect();
  if (
    style.display === "none"
    || style.visibility === "hidden"
    || Number(style.opacity || 1) === 0
    || rect.width <= 0
    || rect.height <= 0
  ) {
    return result("stale_target", "The target is no longer visible.");
  }

  const action = payload.action;
  const targetDomFocused = doc.activeElement === element
    && frameChain.every((entry) => entry.doc.activeElement === entry.frame);
  const targetFocused = targetDomFocused && doc.hasFocus()
    && frameChain.every((entry) => entry.doc.hasFocus());
  let pendingFillInput = null;
  if (["verify", "required"].includes(payload.focusStage) && !targetFocused) {
    pendingFillInput = "Waiting for the target to receive native keyboard focus.";
  }
  if (action.type === "fill") {
    const textInput = element instanceof view.HTMLInputElement
      && ["text", "search", "url", "tel"].includes(fresh.inputType);
    const fillable = textInput || element instanceof view.HTMLTextAreaElement;
    if (!fillable) {
      return result("unsupported_native", "Native fill supports ordinary text inputs and textareas.");
    }
    if (element.readOnly) return result("invalid_value", "The target is read-only.");
    if (["select_all", "insert"].includes(payload.fillStage) && !targetFocused) {
      pendingFillInput = "Waiting for the target to receive native focus.";
    }
    const current = element.value;
    if (payload.fillStage === "insert" && (
      element.selectionStart !== 0 || element.selectionEnd !== String(current || "").length
    )) {
      pendingFillInput = "Waiting for the target to select its text.";
    }
    if (String(current || "") === action.value) {
      return result("no_op", "The target already contains the requested value.");
    }
    if (payload.fillStage === "verify") {
      pendingFillInput = "Waiting for the requested field value.";
    }
  } else if (action.type === "select") {
    if (!(element instanceof view.HTMLSelectElement)) {
      return result("unsupported_native", "This target is not a select control.");
    }
    const options = Array.from(element.options);
    const optionIndex = options.findIndex((option) => option.value === action.value);
    if (optionIndex < 0) {
      return result("invalid_value", "The requested option is not present.");
    }
    const option = options[optionIndex];
    if (option.disabled) return result("invalid_value", "The requested option is disabled.");
    if (element.value === action.value) {
      return result("no_op", "The requested option is already selected.");
    }
    if (!element.multiple && element.size <= 1) {
      return result(
        "unsupported_native",
        "This select opens a native popup. Take over the browser to choose an option. No input was sent.",
      );
    }
    payload.optionIndex = optionIndex;
    payload.selectedIndex = element.selectedIndex;
    if (payload.previousSelectedIndex !== null
      && payload.previousSelectedIndex !== undefined
      && element.selectedIndex === payload.previousSelectedIndex) {
      pendingFillInput = "Waiting for the select control to process native input.";
    }
  } else if (action.type === "check") {
    if (
      !(element instanceof view.HTMLInputElement)
      || !["checkbox", "radio"].includes(element.type)
    ) {
      return result("unsupported_native", "This target is not a checkbox or radio control.");
    }
    if (element.type === "radio" && !action.checked) {
      return result("invalid_value", "A radio control cannot be cleared with native input.");
    }
    if (Boolean(element.checked) === Boolean(action.checked)) {
      return result("no_op", "The target already has the requested checked state.");
    }
  } else if (action.type === "focus" && targetFocused) {
    return result("no_op", "The target already has focus.");
  }

  const visibleTopRect = (frameDepth, contextOffsetX, contextOffsetY, view) => {
    let visible = translate(viewportRect(view), contextOffsetX, contextOffsetY);
    for (let index = 0; visible && index < frameDepth; index += 1) {
      const entry = frameChain[index];
      const parentView = entry.doc.defaultView;
      if (!parentView) return null;
      const frameRect = clientRect(entry.frame);
      const clipped = clippedRect(entry.frame, parentView, frameRect);
      visible = clipped
        ? intersect(visible, translate(clipped, entry.offsetX, entry.offsetY))
        : null;
    }
    return visible;
  };
  const scrollPlanFor = (context) => {
    const view = context.doc.defaultView;
    if (!view) return null;
    const targetRect = context.subject.getBoundingClientRect();
    const targetCenterX = targetRect.left + targetRect.width / 2;
    const targetCenterY = targetRect.top + targetRect.height / 2;
    const topVisible = visibleTopRect(
      context.frameDepth,
      context.offsetX,
      context.offsetY,
      view,
    );
    if (!topVisible) return null;

    for (
      let ancestor = context.subject.parentElement;
      ancestor;
      ancestor = ancestor.parentElement
    ) {
      const style = view.getComputedStyle(ancestor);
      const maximumX = Math.max(0, Number(ancestor.scrollWidth || 0) - Number(ancestor.clientWidth || 0));
      const maximumY = Math.max(0, Number(ancestor.scrollHeight || 0) - Number(ancestor.clientHeight || 0));
      const canScrollX = scrollsAxis(style.overflowX) && maximumX >= 1;
      const canScrollY = scrollsAxis(style.overflowY) && maximumY >= 1;
      if (!canScrollX && !canScrollY) continue;
      const client = clientRect(ancestor);
      const deltaX = canScrollX
        ? clampScrollDelta(
          targetCenterX - (client.left + client.right) / 2,
          Number(ancestor.scrollLeft || 0),
          maximumX,
        )
        : 0;
      const deltaY = canScrollY
        ? clampScrollDelta(
          targetCenterY - (client.top + client.bottom) / 2,
          Number(ancestor.scrollTop || 0),
          maximumY,
        )
        : 0;
      if (Math.abs(deltaX) < 1 && Math.abs(deltaY) < 1) continue;
      const localVisible = clippedRect(ancestor, view, client);
      if (!localVisible) continue;
      const topContainerVisible = intersect(
        translate(localVisible, context.offsetX, context.offsetY),
        topVisible,
      );
      if (!topContainerVisible) continue;
      const point = eventPoint(
        ancestor,
        translate(topContainerVisible, -context.offsetX, -context.offsetY),
        context.doc,
      );
      if (!point) continue;
      return {
        x: context.offsetX + point.x,
        y: context.offsetY + point.y,
        deltaX,
        deltaY,
      };
    }

    const scrollingElement = context.doc.scrollingElement || context.doc.documentElement;
    if (!scrollingElement) return null;
    const maximumX = Math.max(0, Number(scrollingElement.scrollWidth || 0) - view.innerWidth);
    const maximumY = Math.max(0, Number(scrollingElement.scrollHeight || 0) - view.innerHeight);
    const currentX = Number(view.scrollX || scrollingElement.scrollLeft || 0);
    const currentY = Number(view.scrollY || scrollingElement.scrollTop || 0);
    const deltaX = clampScrollDelta(targetCenterX - view.innerWidth / 2, currentX, maximumX);
    const deltaY = clampScrollDelta(targetCenterY - view.innerHeight / 2, currentY, maximumY);
    if (Math.abs(deltaX) < 1 && Math.abs(deltaY) < 1) return null;
    const localVisible = translate(topVisible, -context.offsetX, -context.offsetY);
    const point = eventPoint(null, localVisible, context.doc);
    if (!point) return null;
    return {
      x: context.offsetX + point.x,
      y: context.offsetY + point.y,
      deltaX,
      deltaY,
    };
  };

  if (action.type === "scroll_into_view") {
    const contexts = [{
      doc,
      subject: element,
      offsetX,
      offsetY,
      frameDepth: frameChain.length,
    }];
    for (let index = frameChain.length - 1; index >= 0; index -= 1) {
      const entry = frameChain[index];
      contexts.push({
        doc: entry.doc,
        subject: entry.frame,
        offsetX: entry.offsetX,
        offsetY: entry.offsetY,
        frameDepth: index,
      });
    }
    for (const context of contexts) {
      const plan = scrollPlanFor(context);
      if (plan) {
        return result("ready", "The target requires native scrolling.", {
          x: offsetX + rect.x,
          y: offsetY + rect.y,
          width: rect.width,
          height: rect.height,
          viewportWidth: window.innerWidth,
          viewportHeight: window.innerHeight,
          scrollX: plan.x,
          scrollY: plan.y,
          scrollDeltaX: plan.deltaX,
          scrollDeltaY: plan.deltaY,
        });
      }
    }
  }

  const localX = rect.left + rect.width / 2;
  const localY = rect.top + rect.height / 2;
  if (localX < 0 || localY < 0 || localX >= view.innerWidth || localY >= view.innerHeight) {
    return result("target_obscured", "The target is outside the visible viewport.");
  }
  const hit = doc.elementFromPoint(localX, localY);
  if (!hit || (hit !== element && !element.contains(hit))) {
    return result("target_obscured", "Another element is covering the target.");
  }
  const topX = offsetX + localX;
  const topY = offsetY + localY;
  for (const entry of frameChain) {
    const parentView = entry.doc.defaultView;
    if (!parentView) return result("stale_target", "The target frame is unavailable.");
    const parentX = topX - entry.offsetX;
    const parentY = topY - entry.offsetY;
    if (
      parentX < 0
      || parentY < 0
      || parentX >= parentView.innerWidth
      || parentY >= parentView.innerHeight
    ) {
      return result("target_obscured", "The target frame is outside the visible viewport.");
    }
    const parentHit = entry.doc.elementFromPoint(parentX, parentY);
    if (!parentHit || (parentHit !== entry.frame && !entry.frame.contains(parentHit))) {
      return result("target_obscured", "Another element is covering the target frame.");
    }
  }

  if (action.type === "scroll_into_view") {
    return result("no_op", "The target is visible after native scrolling.");
  }

  if (pendingFillInput) return result("pending_native_input", pendingFillInput);

  return result("ready", "The target is ready for native input.", {
    x: offsetX + rect.x,
    y: offsetY + rect.y,
    width: rect.width,
    height: rect.height,
    viewportWidth: window.innerWidth,
    viewportHeight: window.innerHeight,
    optionIndex: payload.optionIndex ?? null,
    selectedIndex: payload.selectedIndex ?? null,
    targetFocused,
    targetDomFocused,
  });
})()
"#;

const BROWSER_UPLOAD_SCRIPT: &str = r#"
(() => {
  const payload = __PAYLOAD__;
  __TARGET_IDENTITY_STORE__
  const clean = (value, limit = 240) => String(value || "")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, limit);
  __SENSITIVE_FIELD_POLICY__
  const roleFor = (element) => {
    const explicit = clean(element.getAttribute("role"), 60);
    if (explicit) return explicit;
    const tag = element.localName;
    const type = clean(element.getAttribute("type"), 40).toLowerCase();
    if (tag === "input") {
      if (type === "checkbox") return "checkbox";
      if (type === "radio") return "radio";
      if (["button", "submit", "reset", "image"].includes(type)) return "button";
      return "textbox";
    }
    return tag || "element";
  };
  const result = (status, message) => JSON.stringify({ status, message });

  let doc = document;
  for (const selector of payload.framePath) {
    const frame = doc.querySelector(selector);
    if (!frame) return result("stale_target", "The target frame changed.");
    try {
      doc = frame.contentDocument;
    } catch (_) {
      return result("invalid_target", "The target is inside a cross-origin frame.");
    }
    if (!doc) return result("stale_target", "The target frame is unavailable.");
  }

  const element = doc.querySelector(payload.selector);
  if (!element || !element.isConnected) {
    return result("stale_target", "The target no longer exists.");
  }
  const targetIdentity = tidebreakTargetIdentityStore.get(element);
  if (
    !targetIdentity
    || targetIdentity.snapshotMarker !== payload.marker
    || targetIdentity.targetRef !== payload.markerValue
  ) {
    return result("stale_target", "The target element was replaced.");
  }

  const view = doc.defaultView;
  if (!view) return result("stale_target", "The target document is unavailable.");
  const isSensitive = tidebreakIsSensitiveField(element, doc);
  const fresh = {
    tag: element.localName || "element",
    role: roleFor(element),
    name: isSensitive ? "Sensitive field" : tidebreakAccessibleName(element, doc),
    inputType: element.localName === "input"
      ? clean(element.getAttribute("type") || "text", 40).toLowerCase()
      : null,
    href: element.href ? clean(element.href, 2048) : null,
    sensitive: isSensitive,
  };
  if (
    fresh.tag !== payload.fingerprint.tag
    || fresh.role !== payload.fingerprint.role
    || fresh.name !== payload.fingerprint.name
    || fresh.inputType !== payload.fingerprint.inputType
    || fresh.href !== payload.fingerprint.href
    || fresh.sensitive !== payload.fingerprint.sensitive
  ) {
    return result("stale_target", "The target's identifying content changed.");
  }
  if (
    !(element instanceof view.HTMLInputElement)
    || fresh.inputType !== "file"
    || !fresh.sensitive
  ) {
    return result("invalid_target", "The selected target is not a file input.");
  }
  if (element.disabled || element.getAttribute("aria-disabled") === "true") {
    return result("invalid_target", "The file input is disabled.");
  }

  try {
    if (
      typeof view.File !== "function"
      || typeof view.DataTransfer !== "function"
      || typeof view.Event !== "function"
    ) {
      return result("engine_failure", "This browser cannot attach a file to the selected input.");
    }
    const decoded = view.atob(payload.file.contentBase64);
    if (decoded.length !== payload.file.byteLen) {
      return result("engine_failure", "The confirmed file bytes could not be decoded.");
    }
    const bytes = new Uint8Array(decoded.length);
    for (let index = 0; index < decoded.length; index += 1) {
      bytes[index] = decoded.charCodeAt(index);
    }
    const file = new view.File([bytes], payload.file.name, {
      type: payload.file.mediaType,
      lastModified: 0,
    });
    const transfer = new view.DataTransfer();
    transfer.items.add(file);
    const descriptor = Object.getOwnPropertyDescriptor(
      view.HTMLInputElement.prototype,
      "files",
    );
    if (!descriptor || typeof descriptor.set !== "function") {
      return result("engine_failure", "This browser cannot set the selected input's files.");
    }
    descriptor.set.call(element, transfer.files);
    if (
      !element.files
      || element.files.length !== 1
      || element.files[0].name !== payload.file.name
      || element.files[0].size !== payload.file.byteLen
    ) {
      return result("engine_failure", "The browser did not retain the confirmed file.");
    }
    element.dispatchEvent(new view.Event("input", { bubbles: true, composed: true }));
    element.dispatchEvent(new view.Event("change", { bubbles: true, composed: true }));
    return result("uploaded", "The confirmed file was attached.");
  } catch (_) {
    return result("engine_failure", "The browser could not attach the confirmed file.");
  }
})()
"#;

pub(crate) async fn browser_inject_inspect_overlay(
    app: &tauri::AppHandle,
    registry: &crate::browser_control::BrowserRegistry,
    browser_id: &str,
    workspace_id: &str,
) -> Result<(), String> {
    registry.ensure_workspace(browser_id, workspace_id)?;
    let snapshot = registry.snapshot(browser_id, workspace_id)?;
    if !snapshot
        .engine
        .as_ref()
        .is_some_and(|engine| engine.capabilities.screenshot)
    {
        return Err(
            "browser inspect requires human takeover because this engine cannot prove closed-shadow privacy"
                .to_owned(),
        );
    }
    let label = crate::code_browser::browser_label(browser_id)?;
    let webview = app
        .get_webview(&label)
        .ok_or_else(|| "browser session is not open".to_owned())?;

    let _result: String = evaluate_json(&webview, &inspect_overlay_script()).await?;
    Ok(())
}

pub(crate) async fn browser_remove_inspect_overlay(
    app: &tauri::AppHandle,
    registry: &crate::browser_control::BrowserRegistry,
    browser_id: &str,
    workspace_id: &str,
) -> Result<(), String> {
    registry.ensure_workspace(browser_id, workspace_id)?;
    let label = crate::code_browser::browser_label(browser_id)?;
    let webview = app
        .get_webview(&label)
        .ok_or_else(|| "browser session is not open".to_owned())?;

    let _result: String = evaluate_json(&webview, REMOVE_INSPECT_OVERLAY_SCRIPT).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    static SOURCE: &str = include_str!("browser_semantics.rs");

    fn native_resolution() -> NativeActionResolution {
        NativeActionResolution {
            status: NativeActionResolutionStatus::Ready,
            message: "ready".to_owned(),
            url: "https://example.com/".to_owned(),
            title: "Example".to_owned(),
            x: Some(10.0),
            y: Some(20.0),
            width: Some(100.0),
            height: Some(30.0),
            viewport_width: Some(800.0),
            viewport_height: Some(600.0),
            option_index: None,
            selected_index: None,
            scroll_x: None,
            scroll_y: None,
            scroll_delta_x: None,
            scroll_delta_y: None,
            target_focused: false,
            target_dom_focused: false,
        }
    }

    #[test]
    fn snapshot_bounds_use_the_shared_contract_before_script_generation() {
        let arguments = BrowserSnapshotArgs {
            browser_id: "browser-1".to_owned(),
            max_nodes: Some(MAX_BROWSER_SNAPSHOT_NODES),
        };
        assert!(arguments.is_well_formed());
        let max_nodes = arguments.bounded_max_nodes();
        assert!(snapshot_script(max_nodes, "__marker").contains("const MAX_NODES = 500;"));
    }

    #[test]
    fn action_values_are_json_encoded_instead_of_interpolated_as_code() {
        let target = BrowserTargetRecord {
            frame_path: vec!["iframe:nth-of-type(1)".to_owned()],
            selector: "input:nth-of-type(1)".to_owned(),
            marker: "__marker".to_owned(),
            marker_value: "@e1".to_owned(),
            fingerprint: BrowserTargetFingerprint {
                tag: "input".to_owned(),
                role: "textbox".to_owned(),
                name: "Search".to_owned(),
                input_type: Some("text".to_owned()),
                href: None,
                sensitive: false,
            },
            sensitive: false,
            consequential: false,
        };
        let script = native_action_resolution_script(
            &target,
            &tidebreak_core::BrowserAction::Fill {
                value: "'); globalThis.pwned = true; ('".to_owned(),
            },
        )
        .unwrap();
        assert!(script.contains("globalThis.pwned"));
        assert!(script.contains("const payload = {"));
        assert!(!script.contains("value: '); globalThis.pwned"));
    }

    #[test]
    fn page_text_cannot_replace_the_shared_policy_placeholder() {
        let target = BrowserTargetRecord {
            frame_path: vec![],
            selector: "input:nth-of-type(1)".to_owned(),
            marker: "__marker".to_owned(),
            marker_value: "@e1".to_owned(),
            fingerprint: BrowserTargetFingerprint {
                tag: "input".to_owned(),
                role: "textbox".to_owned(),
                name: "__SENSITIVE_FIELD_POLICY__".to_owned(),
                input_type: Some("text".to_owned()),
                href: None,
                sensitive: false,
            },
            sensitive: false,
            consequential: false,
        };

        let script =
            native_action_resolution_script(&target, &tidebreak_core::BrowserAction::Focus)
                .unwrap();
        assert!(script.contains(r#""name":"__SENSITIVE_FIELD_POLICY__""#));
        assert_eq!(script.matches("const tidebreakIsSensitiveField").count(), 1);
    }

    #[test]
    fn snapshots_label_page_content_as_untrusted() {
        assert_eq!(
            serde_json::to_value(BrowserContentTrust::UntrustedPage).unwrap(),
            serde_json::json!("untrusted_page")
        );
    }

    #[test]
    fn snapshot_script_collects_bounded_static_content_without_refs() {
        let script = snapshot_script(25, "__marker");
        assert!(script.contains("const CONTENT_SELECTOR"));
        assert!(script.contains("interactive ? `@e${nextTargetRef++}` : null"));
        assert!(script.contains("kind: interactive ? \"interactive\" : \"content\""));
        assert!(script.contains("if (!interactive && !text) continue"));
        assert!(script.contains("type === \"file\""));
        assert!(script.contains("const isConsequential"));
        assert!(script.contains("[\"focus\", \"hover\", \"scroll_into_view\"]"));
    }

    #[test]
    fn target_identity_stays_in_the_private_content_world() {
        let snapshot = snapshot_script(25, "__marker");
        assert!(snapshot.contains("const store = new WeakMap()"));
        assert!(snapshot.contains("tidebreakTargetIdentityStore.set(element"));
        assert!(!snapshot.contains("Object.defineProperty(element"));

        let target = BrowserTargetRecord {
            frame_path: vec![],
            selector: "button:nth-of-type(1)".to_owned(),
            marker: "__marker".to_owned(),
            marker_value: "@e1".to_owned(),
            fingerprint: BrowserTargetFingerprint {
                tag: "button".to_owned(),
                role: "button".to_owned(),
                name: "Continue".to_owned(),
                input_type: None,
                href: None,
                sensitive: false,
            },
            sensitive: false,
            consequential: false,
        };
        let action =
            native_action_resolution_script(&target, &tidebreak_core::BrowserAction::Hover)
                .unwrap();
        assert!(action.contains("tidebreakTargetIdentityStore.get(element)"));
        assert!(!action.contains("element[payload.marker]"));
    }

    #[test]
    fn hover_is_encoded_as_a_native_pointer_action() {
        let target = BrowserTargetRecord {
            frame_path: vec![],
            selector: "button:nth-of-type(1)".to_owned(),
            marker: "__marker".to_owned(),
            marker_value: "@e1".to_owned(),
            fingerprint: BrowserTargetFingerprint {
                tag: "button".to_owned(),
                role: "button".to_owned(),
                name: "Preview".to_owned(),
                input_type: None,
                href: None,
                sensitive: false,
            },
            sensitive: false,
            consequential: false,
        };
        let script =
            native_action_resolution_script(&target, &tidebreak_core::BrowserAction::Hover)
                .unwrap();
        assert!(script.contains(r#""action":{"type":"hover"}"#));
    }

    #[test]
    fn external_selects_require_confirmation_before_native_input() {
        let external = BrowserOrigin::from_url("https://example.com/form").unwrap();
        let loopback = BrowserOrigin::from_url("http://127.0.0.1:4173/form").unwrap();
        let select = tidebreak_core::BrowserAction::Select {
            value: "publish".to_owned(),
        };
        let click = tidebreak_core::BrowserAction::Click;

        assert!(native_action_requires_confirmation(
            &external, false, &select
        ));
        assert!(!native_action_requires_confirmation(
            &loopback, true, &select
        ));
        assert!(!native_action_requires_confirmation(
            &external, false, &click
        ));
        assert!(native_action_requires_confirmation(&external, true, &click));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn dropping_a_native_input_future_disarms_its_queued_callback() {
        use std::sync::{Arc, Mutex};
        let (sender, mut receiver) = oneshot::channel();
        let state = Arc::new(Mutex::new(NativeInputCallbackState {
            sender: Some(sender),
            cancelled: false,
        }));
        {
            let _cancellation = NativeInputCancellation(Arc::clone(&state));
            assert!(!state.lock().unwrap().cancelled);
        }
        let callback = state.lock().unwrap();
        assert!(callback.cancelled);
        assert!(callback.sender.is_none());
        assert!(matches!(
            receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
    }

    struct QueuedUploadFixture {
        authorization: NativeUploadAuthorization,
        state: std::sync::Arc<std::sync::Mutex<NativeUploadCallbackState>>,
        receiver: oneshot::Receiver<Result<String, String>>,
        _private: tempfile::TempDir,
    }

    fn queued_upload_fixture() -> QueuedUploadFixture {
        use tidebreak_core::{BrowserOriginScope, BrowserUploadArgs, BrowserUploadResource};

        let registry = BrowserRegistry::default();
        let private = tempfile::tempdir().unwrap();
        registry.initialize_private_state(private.path()).unwrap();
        let origin = BrowserOrigin::from_url("https://example.com/upload").unwrap();
        let instance = registry
            .register("browser-1", "workspace-1", origin.as_str().to_owned(), true)
            .unwrap();
        registry
            .page_finished(
                "browser-1",
                "workspace-1",
                instance,
                origin.as_str().to_owned(),
            )
            .unwrap();
        registry
            .grant_browser_access(
                "browser-1",
                "workspace-1",
                &origin,
                BrowserOriginScope::Origin {
                    origin: origin.clone(),
                },
                &[
                    BrowserGrantCapability::BrowserControlOrigin,
                    BrowserGrantCapability::BrowserTransferFiles,
                ],
            )
            .unwrap();
        let capability_id = registry.issue_agent_capability("workspace-1", "Code agent");
        registry
            .begin_agent_control(capability_id, "browser-1")
            .unwrap();
        let target = BrowserTargetRecord {
            frame_path: vec![],
            selector: "input:nth-of-type(1)".to_owned(),
            marker: "__marker".to_owned(),
            marker_value: "@e1".to_owned(),
            fingerprint: BrowserTargetFingerprint {
                tag: "input".to_owned(),
                role: "textbox".to_owned(),
                name: "File input".to_owned(),
                input_type: Some("file".to_owned()),
                href: None,
                sensitive: true,
            },
            sensitive: true,
            consequential: true,
        };
        registry
            .record_semantic_snapshot(
                "browser-1",
                "workspace-1",
                0,
                "snapshot-1".to_owned(),
                HashMap::from([("@e1".to_owned(), target.clone())]),
            )
            .unwrap();
        let fence = registry
            .observation_fence(capability_id, "browser-1")
            .unwrap();
        let authorization = NativeUploadAuthorization {
            registry,
            capability_id,
            workspace_id: "workspace-1".to_owned(),
            origin,
            fence,
            arguments: BrowserUploadArgs {
                browser_id: "browser-1".to_owned(),
                snapshot_id: "snapshot-1".to_owned(),
                document_epoch: 0,
                target_ref: "@e1".to_owned(),
                resource: BrowserUploadResource::Output {
                    output_id: Uuid::new_v4(),
                },
            },
            target,
        };
        authorization.authorize().unwrap();
        let (sender, receiver) = oneshot::channel();
        QueuedUploadFixture {
            authorization,
            state: std::sync::Arc::new(std::sync::Mutex::new(NativeUploadCallbackState {
                sender: Some(sender),
                cancelled: false,
                deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(10),
            })),
            receiver,
            _private: private,
        }
    }

    #[tokio::test]
    async fn queued_upload_is_disarmed_when_its_operation_future_is_dropped() {
        let mut fixture = queued_upload_fixture();
        let future_state = std::sync::Arc::clone(&fixture.state);
        let (started, ready) = oneshot::channel();
        let operation = tokio::spawn(async move {
            let _cancellation = NativeUploadCancellation(future_state);
            started.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        ready.await.unwrap();
        operation.abort();
        assert!(operation.await.unwrap_err().is_cancelled());

        let mut submissions = 0;
        let error = submit_native_browser_upload(&fixture.state, &fixture.authorization, || {
            submissions += 1;
        })
        .unwrap_err();
        assert_eq!(error, "browser upload was cancelled");
        assert_eq!(submissions, 0);
        assert!(matches!(
            fixture.receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
    }

    #[tokio::test]
    async fn queued_upload_rechecks_native_authority_before_submitting() {
        for changed in [
            "capability",
            "expired",
            "hidden",
            "stop",
            "human",
            "document",
            "instance",
            "sharing",
        ] {
            let fixture = queued_upload_fixture();
            let authorization = &fixture.authorization;
            let registry = &authorization.registry;
            match changed {
                "capability" => registry.revoke_agent_capability(authorization.capability_id),
                "expired" => registry.expire_agent_capability_for_test(authorization.capability_id),
                "hidden" => {
                    registry
                        .set_visible("browser-1", "workspace-1", false)
                        .unwrap();
                }
                "stop" => {
                    registry
                        .stop_agent_control("browser-1", "workspace-1")
                        .await
                        .unwrap();
                }
                "human" => {
                    registry
                        .take_human_control("browser-1", "workspace-1")
                        .await
                        .unwrap();
                }
                "document" => {
                    registry
                        .page_started(
                            "browser-1",
                            "workspace-1",
                            authorization.fence.instance_id,
                            "https://example.com/changed".to_owned(),
                        )
                        .unwrap();
                }
                "instance" => {
                    registry.remove("browser-1", "workspace-1").unwrap();
                    let instance = registry
                        .register(
                            "browser-1",
                            "workspace-1",
                            authorization.origin.as_str().to_owned(),
                            true,
                        )
                        .unwrap();
                    registry
                        .page_finished(
                            "browser-1",
                            "workspace-1",
                            instance,
                            authorization.origin.as_str().to_owned(),
                        )
                        .unwrap();
                    registry
                        .begin_agent_control(authorization.capability_id, "browser-1")
                        .unwrap();
                }
                "sharing" => {
                    registry
                        .revoke_browser_access("browser-1", "workspace-1")
                        .unwrap();
                }
                _ => unreachable!(),
            }
            let mut submissions = 0;
            assert!(
                submit_native_browser_upload(&fixture.state, authorization, || {
                    submissions += 1;
                })
                .is_err(),
                "accepted changed {changed}"
            );
            assert_eq!(submissions, 0, "submitted after changed {changed}");
        }
    }

    #[test]
    fn queued_upload_requires_transfer_permission_even_when_control_remains_valid() {
        let fixture = queued_upload_fixture();
        let authorization = &fixture.authorization;
        let registry = &authorization.registry;
        registry
            .revoke_browser_access("browser-1", "workspace-1")
            .unwrap();
        registry
            .grant_browser_access(
                "browser-1",
                "workspace-1",
                &authorization.origin,
                tidebreak_core::BrowserOriginScope::Origin {
                    origin: authorization.origin.clone(),
                },
                &[BrowserGrantCapability::BrowserControlOrigin],
            )
            .unwrap();
        registry
            .begin_agent_control(authorization.capability_id, "browser-1")
            .unwrap();
        registry
            .record_semantic_snapshot(
                "browser-1",
                "workspace-1",
                0,
                "snapshot-1".to_owned(),
                HashMap::from([("@e1".to_owned(), authorization.target.clone())]),
            )
            .unwrap();
        registry
            .authorize_native_action_phase(
                authorization.capability_id,
                "browser-1",
                "workspace-1",
                &authorization.origin,
                authorization.fence,
            )
            .unwrap();

        let mut submissions = 0;
        let error = submit_native_browser_upload(&fixture.state, authorization, || {
            submissions += 1;
        })
        .unwrap_err();
        assert_eq!(error, "browser origin is not shared for this operation");
        assert_eq!(submissions, 0);
    }

    #[test]
    fn queued_upload_rejects_a_replaced_snapshot_or_target() {
        for changed in ["snapshot", "target"] {
            let fixture = queued_upload_fixture();
            let authorization = &fixture.authorization;
            let mut target = authorization.target.clone();
            if changed == "target" {
                target.marker_value = "@replacement".to_owned();
            }
            authorization
                .registry
                .record_semantic_snapshot(
                    "browser-1",
                    "workspace-1",
                    0,
                    if changed == "snapshot" {
                        "snapshot-2"
                    } else {
                        "snapshot-1"
                    }
                    .to_owned(),
                    HashMap::from([("@e1".to_owned(), target)]),
                )
                .unwrap();
            let mut submissions = 0;
            assert!(
                submit_native_browser_upload(&fixture.state, authorization, || {
                    submissions += 1;
                })
                .is_err(),
                "accepted changed {changed}"
            );
            assert_eq!(submissions, 0);
        }
    }

    #[test]
    fn queued_upload_cannot_submit_after_its_deadline_or_receiver_closes() {
        for changed in ["deadline", "receiver"] {
            let fixture = queued_upload_fixture();
            if changed == "deadline" {
                fixture.state.lock().unwrap().deadline = tokio::time::Instant::now();
            } else {
                drop(fixture.receiver);
            }
            let mut submissions = 0;
            assert!(
                submit_native_browser_upload(&fixture.state, &fixture.authorization, || {
                    submissions += 1;
                })
                .is_err(),
                "accepted changed {changed}"
            );
            assert_eq!(submissions, 0);
        }
    }

    #[test]
    fn queued_upload_submits_once_with_live_consent_and_the_same_target() {
        let fixture = queued_upload_fixture();
        let mut submissions = 0;
        submit_native_browser_upload(&fixture.state, &fixture.authorization, || {
            submissions += 1;
        })
        .unwrap();
        assert_eq!(submissions, 1);
    }

    #[test]
    fn fill_waits_for_focus_and_selection_before_inserting() {
        let action = tidebreak_core::BrowserAction::Fill {
            value: "task".to_owned(),
        };
        assert_eq!(
            native_pending_event_kind(&action, NativeActionDispatchPhase::Initial),
            NativePendingEventKind::Presentation
        );
        assert_eq!(
            native_pending_event_kind(&action, NativeActionDispatchPhase::FillSelectAll),
            NativePendingEventKind::Presentation
        );
        assert_eq!(
            native_pending_event_kind(&action, NativeActionDispatchPhase::FillInsert),
            NativePendingEventKind::Presentation
        );
        assert_eq!(
            native_pending_event_kind(&action, NativeActionDispatchPhase::FillVerify),
            NativePendingEventKind::None
        );
    }

    #[tokio::test]
    async fn fill_polls_pending_state_and_stops_after_one_ready_result() {
        let mut calls = 0;
        let resolution = wait_for_native_action_phase(
            NativeActionDispatchPhase::FillInsert,
            std::time::Duration::from_secs(1),
            |_| {
                calls += 1;
                let mut resolution = native_resolution();
                if calls < 3 {
                    resolution.status = NativeActionResolutionStatus::PendingNativeInput;
                }
                std::future::ready(Ok(resolution))
            },
        )
        .await
        .unwrap();
        assert_eq!(resolution.status, NativeActionResolutionStatus::Ready);
        assert_eq!(calls, 3);
    }

    #[tokio::test]
    async fn fill_preserves_input_completed_while_the_native_deadline_fires() {
        let resolution = wait_for_native_action_phase(
            NativeActionDispatchPhase::FillInsert,
            std::time::Duration::from_millis(5),
            |deadline| async move {
                // A native callback can hold its mutex through the deadline.
                // The resolver returns that completed result after dispatch.
                tokio::time::sleep_until(deadline + std::time::Duration::from_millis(5)).await;
                Ok(native_resolution())
            },
        )
        .await
        .unwrap();
        assert_eq!(resolution.status, NativeActionResolutionStatus::Ready);
    }

    #[tokio::test]
    async fn fill_does_not_poll_again_after_a_fresh_target_refusal() {
        let mut calls = 0;
        let resolution = wait_for_native_action_phase(
            NativeActionDispatchPhase::FillInsert,
            std::time::Duration::from_secs(1),
            |_| {
                calls += 1;
                let mut resolution = native_resolution();
                resolution.status = NativeActionResolutionStatus::StaleTarget;
                std::future::ready(Ok(resolution))
            },
        )
        .await
        .unwrap();
        assert_eq!(resolution.status, NativeActionResolutionStatus::StaleTarget);
        assert_eq!(calls, 1);
    }

    #[tokio::test]
    async fn fill_bounds_pending_and_unresponsive_postconditions() {
        for phase in [
            NativeActionDispatchPhase::FillSelectAll,
            NativeActionDispatchPhase::FillInsert,
            NativeActionDispatchPhase::FillVerify,
        ] {
            let failure = wait_for_native_action_phase(
                phase,
                std::time::Duration::from_millis(5),
                |deadline| async move {
                    tokio::time::sleep_until(deadline).await;
                    Err(native_input_deadline_failure(phase, Some(deadline)))
                },
            )
            .await
            .unwrap_err();
            let expected = if matches!(phase, NativeActionDispatchPhase::FillVerify) {
                tidebreak_core::BrowserActStatus::EngineFailure
            } else {
                tidebreak_core::BrowserActStatus::UnsupportedNative
            };
            assert_eq!(failure.status(), expected);
        }
        let mut calls = 0;
        assert!(wait_for_native_action_phase(
            NativeActionDispatchPhase::FillInsert,
            std::time::Duration::from_millis(5),
            |_| {
                calls += 1;
                let mut resolution = native_resolution();
                resolution.status = NativeActionResolutionStatus::PendingNativeInput;
                std::future::ready(Ok(resolution))
            },
        )
        .await
        .is_err());
        assert_eq!(
            calls, 1,
            "an expired phase must not start another resolver callback"
        );
    }

    #[test]
    fn keyboard_dispatch_requires_native_focus_after_acquisition() {
        for phase in [
            NativeActionDispatchPhase::PressKey,
            NativeActionDispatchPhase::SelectInitialStep,
        ] {
            let mut resolution = native_resolution();
            resolution.target_dom_focused = true;
            assert_eq!(
                validate_native_follow_up_progress(phase, &resolution)
                    .unwrap_err()
                    .status(),
                tidebreak_core::BrowserActStatus::UnsupportedNative
            );
            resolution.target_focused = true;
            assert!(validate_native_follow_up_progress(phase, &resolution).is_ok());
        }
        let mut resolution = native_resolution();
        resolution.selected_index = Some(2);
        resolution.option_index = Some(4);
        let phase = NativeActionDispatchPhase::SelectFollowUp {
            previous_selected_index: 1,
            previous_distance: 3,
        };
        assert_eq!(
            validate_native_follow_up_progress(phase, &resolution)
                .unwrap_err()
                .status(),
            tidebreak_core::BrowserActStatus::UnsupportedNative
        );
        resolution.target_focused = true;
        assert!(validate_native_follow_up_progress(phase, &resolution).is_ok());
    }

    #[test]
    fn keyboard_actions_wait_after_focus_and_each_dispatched_key() {
        for action in [
            tidebreak_core::BrowserAction::Focus,
            tidebreak_core::BrowserAction::Press {
                key: "Enter".to_owned(),
            },
            tidebreak_core::BrowserAction::Select {
                value: "two".to_owned(),
            },
        ] {
            assert_eq!(
                native_pending_event_kind(&action, NativeActionDispatchPhase::Initial),
                NativePendingEventKind::Presentation
            );
        }
        assert_eq!(
            native_pending_event_kind(
                &tidebreak_core::BrowserAction::Focus,
                NativeActionDispatchPhase::FocusVerify
            ),
            NativePendingEventKind::None
        );
        assert_eq!(
            native_pending_event_kind(
                &tidebreak_core::BrowserAction::Press {
                    key: "Enter".to_owned()
                },
                NativeActionDispatchPhase::PressKey
            ),
            NativePendingEventKind::Presentation
        );
    }

    #[test]
    fn fill_requires_fresh_focus_before_selection_and_insertion() {
        for phase in [
            NativeActionDispatchPhase::FillSelectAll,
            NativeActionDispatchPhase::FillInsert,
        ] {
            let mut resolution = native_resolution();
            let failure = validate_native_follow_up_progress(phase, &resolution).unwrap_err();
            assert_eq!(
                failure.status(),
                tidebreak_core::BrowserActStatus::UnsupportedNative
            );
            resolution.target_focused = true;
            assert!(validate_native_follow_up_progress(phase, &resolution).is_ok());
        }
    }

    #[test]
    fn fill_verification_never_repeats_input_when_the_value_did_not_stick() {
        let mut resolution = native_resolution();
        resolution.target_focused = true;
        let failure =
            validate_native_follow_up_progress(NativeActionDispatchPhase::FillVerify, &resolution)
                .unwrap_err();
        assert_eq!(
            failure.status(),
            tidebreak_core::BrowserActStatus::EngineFailure
        );
        assert!(failure.message().contains("requested field value"));
    }

    #[test]
    fn select_follow_up_requires_progress_toward_the_requested_option() {
        let phase = NativeActionDispatchPhase::SelectFollowUp {
            previous_selected_index: 1,
            previous_distance: 3,
        };
        let mut moving = native_resolution();
        moving.target_focused = true;
        moving.selected_index = Some(2);
        moving.option_index = Some(4);
        assert!(validate_native_follow_up_progress(phase, &moving).is_ok());

        let mut stalled = native_resolution();
        stalled.target_focused = true;
        stalled.selected_index = Some(1);
        stalled.option_index = Some(4);
        let failure = validate_native_follow_up_progress(phase, &stalled).unwrap_err();
        assert_eq!(
            failure.status(),
            tidebreak_core::BrowserActStatus::EngineFailure
        );
        assert!(failure.message().contains("did not move"));

        let mut moving_away = native_resolution();
        moving_away.target_focused = true;
        moving_away.selected_index = Some(0);
        moving_away.option_index = Some(4);
        assert!(validate_native_follow_up_progress(phase, &moving_away).is_err());
    }

    #[test]
    fn scroll_follow_up_fails_when_the_target_and_plan_do_not_move() {
        let phase = NativeActionDispatchPhase::ScrollFollowUp {
            previous_x: 10.0,
            previous_y: 20.0,
            previous_delta_x: 0.0,
            previous_delta_y: 400.0,
        };
        let mut stalled = native_resolution();
        stalled.scroll_delta_x = Some(0.0);
        stalled.scroll_delta_y = Some(400.0);
        let failure = validate_native_follow_up_progress(phase, &stalled).unwrap_err();

        assert_eq!(
            failure.status(),
            tidebreak_core::BrowserActStatus::TargetObscured
        );
        assert!(failure.message().contains("did not move"));

        let mut moving = native_resolution();
        moving.y = Some(-180.0);
        moving.scroll_delta_x = Some(0.0);
        moving.scroll_delta_y = Some(200.0);
        assert!(validate_native_follow_up_progress(phase, &moving).is_ok());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_key_mapping_uses_macos_virtual_keys_and_rejects_chords() {
        assert_eq!(native_key("Enter").unwrap(), (0x24, "\r".to_owned()));
        assert_eq!(native_key("Tab").unwrap(), (0x30, "\t".to_owned()));
        assert_eq!(native_key("Backspace").unwrap(), (0x33, "\u{8}".to_owned()));
        assert_eq!(native_key("ArrowLeft").unwrap().0, 0x7b);
        assert_eq!(native_key("ArrowRight").unwrap().0, 0x7c);
        assert_eq!(native_key("ArrowDown").unwrap().0, 0x7d);
        assert_eq!(native_key("ArrowUp").unwrap().0, 0x7e);
        assert!(native_key("Ctrl+C").is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn select_uses_only_focused_arrow_keys_without_click_or_enter() {
        let source = SOURCE;
        let select_start = source
            .find("unsafe fn send_native_select_step")
            .expect("select helper");
        let select_end = source[select_start..]
            .find("fn send_native_key")
            .map(|offset| select_start + offset)
            .expect("key helper");
        let select = &source[select_start..select_end];
        let finish_start = source
            .find("async fn finish_native_action(")
            .expect("finish helper");
        let finish_end = source[finish_start..]
            .find("async fn continue_native_action(")
            .map(|offset| finish_start + offset)
            .expect("continue helper");
        let finish = &source[finish_start..finish_end];

        assert!(!select.contains("focus_accessibility_target"));
        assert!(select.contains("ArrowDown"));
        assert!(select.contains("ArrowUp"));
        assert!(!select.contains("send_native_click"));
        assert!(!select.contains("\"Enter\""));
        assert!(!finish.contains("send_native_click"));
        assert!(!finish.contains("\"Enter\""));
        let removed_fallback = ["perform_select_key", "_fallback"].concat();
        assert!(!source.contains(&removed_fallback));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scroll_targets_the_window_with_began_changed_and_ended_phases() {
        assert_eq!(
            native_scroll_delta_for_css(50.0, 80.0, 1_600.0, 900.0, 800.0, 600.0).unwrap(),
            (-100, -120)
        );
        assert_eq!(
            native_scroll_event_steps(-100, -120),
            [(1, 0, 0), (2, -120, -100), (4, 0, 0)]
        );

        let source = SOURCE;
        let scroll_start = source
            .find("fn send_native_scroll(")
            .expect("scroll helper");
        let scroll_end = source[scroll_start..]
            .find("fn act_result(")
            .map(|offset| scroll_start + offset)
            .expect("action result helper");
        let scroll = &source[scroll_start..scroll_end];
        assert!(scroll.contains("scroll_x"));
        assert!(scroll.contains("scroll_y"));
        assert!(scroll.contains("_eventRelativeToWindow:"));
        assert!(scroll.contains("window.sendEvent(&relative_event)"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn hover_uses_webkits_native_mouse_move_delivery() {
        let source = SOURCE;
        assert!(source.contains("window.setAcceptsMouseMovedEvents(true)"));
        assert!(source.contains("NSEventType::MouseMoved"));
        assert!(source.contains("window.sendEvent(&event)"));
        assert!(source.contains("_simulateMouseMove:"));
        assert!(source.contains("_doAfterProcessingAllPendingMouseEvents:"));
        assert!(source.contains("native_event_timestamp()"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_input_fails_closed_without_active_target_focus() {
        let source = SOURCE;
        assert!(source.contains("app.isActive()"));
        assert!(source.contains("window.isKeyWindow()"));
        assert!(source.contains("window.isVisible()"));
        assert!(source.contains("window.isMiniaturized()"));
        assert!(source.contains("isAccessibilityFocused"));
        let app_wide_menu_search = ["find_and_press", "_menu_item"].concat();
        assert!(!source.contains(&app_wide_menu_search));
    }

    #[test]
    fn inactive_native_input_returns_hidden_tab_without_invalidating_the_snapshot() {
        let failure = NativeInputFailure::HiddenTab("inactive".to_owned());
        assert_eq!(
            failure.status(),
            tidebreak_core::BrowserActStatus::HiddenTab
        );
        assert!(!failure.requires_resnapshot());
    }

    #[test]
    fn every_browser_surface_uses_the_shared_sensitive_field_policy() {
        let snapshot = snapshot_script(25, "__marker");
        let inspect = inspect_overlay_script();
        let screenshot = screenshot_privacy_script("__watch", false).unwrap();
        let target = BrowserTargetRecord {
            frame_path: vec![],
            selector: "input:nth-of-type(1)".to_owned(),
            marker: "__marker".to_owned(),
            marker_value: "@e1".to_owned(),
            fingerprint: BrowserTargetFingerprint {
                tag: "input".to_owned(),
                role: "textbox".to_owned(),
                name: "Sensitive field".to_owned(),
                input_type: Some("text".to_owned()),
                href: None,
                sensitive: true,
            },
            sensitive: true,
            consequential: false,
        };
        let action =
            native_action_resolution_script(&target, &tidebreak_core::BrowserAction::Focus)
                .unwrap();

        for script in [snapshot, inspect, screenshot, action] {
            assert!(script.contains("const tidebreakIsSensitiveField"));
            assert!(script.contains("aria-describedby"));
            assert!(script.contains("inputmode"));
            assert!(!script.contains("__SENSITIVE_FIELD_POLICY__"));
        }
    }

    #[test]
    fn semantic_scripts_run_outside_the_page_javascript_world() {
        let source = SOURCE;
        assert!(source.contains("TidebreakBrowserSemantics"));
        assert!(source.contains("BROWSER_SEMANTICS_CONTENT_WORLD"));
        assert!(source.contains("evaluateJavaScript_inFrame_inContentWorld_completionHandler"));
    }

    #[test]
    fn same_document_url_changes_satisfy_registry_waits_without_advancing_epoch() {
        let mut snapshot = BrowserSnapshot::missing("browser-1", "workspace-1");
        snapshot.url = Some("https://example.com/?view=details#summary".to_owned());
        snapshot.load_state = Some(BrowserLoadState::Ready);
        snapshot.document_epoch = Some(7);

        assert_eq!(
            registry_wait_condition(
                &tidebreak_core::BrowserWaitCondition::UrlChanged,
                &snapshot,
                Some("https://example.com/"),
            ),
            Some(true)
        );
        assert_eq!(snapshot.document_epoch, Some(7));
    }

    #[test]
    fn sensitive_snapshot_nodes_hide_values_labels_and_actions() {
        let script = snapshot_script(25, "__marker");
        assert!(script.contains("const tidebreakTextWithoutFieldDescendants"));
        assert!(script.contains("node.nodeType === 3"));
        assert!(script.contains("node.matches?.(tidebreakFieldSelector)"));
        assert!(script.contains("sensitive ? \"Sensitive field\""));
        assert!(script.contains("const value = !interactive || sensitive"));
        assert!(script.contains("text: sensitive ? null : (text || null)"));
        assert!(script.contains("interactive && !sensitive && element.href"));
        assert!(script.contains("if (sensitive || nativePopupSelect) return [\"human_takeover\"]"));
    }

    #[test]
    fn actions_and_screenshots_fail_closed_for_sensitive_or_uninspectable_fields() {
        let inspect = inspect_overlay_script();
        let screenshot = screenshot_privacy_script("__watch", false).unwrap();
        assert!(inspect.contains("attachShadow({ mode: \"closed\" })"));
        assert!(inspect.contains("window.requestAnimationFrame(() =>"));
        assert!(inspect.contains("new observed.win.MutationObserver(() => schedule())"));
        assert!(inspect.contains("attributeFilter: OBSERVED_ATTRIBUTES"));
        assert!(inspect.contains("const MAX_OVERLAY_ENTRIES = 500"));
        assert!(inspect.contains("const MAX_OBSERVED_ROOTS = 64"));
        assert!(inspect.contains("const MAX_SCANNED_NODES = 5000"));
        assert!(inspect.contains("createTreeWalker(root, 1)"));
        assert!(inspect.contains("budget.nodes -= 1"));
        assert!(inspect.contains("target.addEventListener(type, schedule, true)"));
        assert!(inspect.contains("state.listeners.push([target, type])"));
        assert!(inspect.contains("listen(observed.win, \"scroll\")"));
        assert!(inspect.contains("listen(observed.win, \"resize\")"));
        assert!(inspect.contains("listen(observedDocument, \"fullscreenchange\")"));
        assert!(inspect.contains("target.removeEventListener(type, schedule, true)"));
        assert!(inspect.contains("addMask(entries, frameRect"));
        assert!(inspect.contains("Sensitive content · human takeover"));
        assert!(screenshot.contains("tidebreakIsSensitiveField(element, doc)"));
        assert!(screenshot.contains("if (tidebreakIsSensitiveField(element, doc))"));
        assert!(screenshot.contains("uninspectableRegions += 1"));
        assert!(screenshot.contains("state.changed = true"));
        assert!(!screenshot.contains("attributeFilter"));
        assert!(screenshot.contains("state.listeners.push([frame, \"load\""));
        assert!(screenshot.contains("state.listeners.push([childWin, \"pagehide\""));

        let target = BrowserTargetRecord {
            frame_path: vec![],
            selector: "input:nth-of-type(1)".to_owned(),
            marker: "__marker".to_owned(),
            marker_value: "@e1".to_owned(),
            fingerprint: BrowserTargetFingerprint {
                tag: "input".to_owned(),
                role: "textbox".to_owned(),
                name: "Sensitive field".to_owned(),
                input_type: Some("text".to_owned()),
                href: None,
                sensitive: true,
            },
            sensitive: true,
            consequential: false,
        };
        let action =
            native_action_resolution_script(&target, &tidebreak_core::BrowserAction::Focus)
                .unwrap();
        assert!(action.contains("human_takeover_required"));
    }

    #[test]
    fn browser_fixture_covers_unannotated_codes_and_ordinary_numbers() {
        let fixture = include_str!("../tests/browser-fixture/index.html");
        for sensitive in [
            "name=\"code\" inputmode=\"numeric\"",
            "id=\"verification-code\" inputmode=\"numeric\"",
            "placeholder=\"Recovery code\"",
            "blockquote data-sensitive-descendant",
        ] {
            assert!(fixture.contains(sensitive));
        }
        for ordinary in [
            "name=\"quantity\" type=\"number\"",
            "name=\"zipCode\" inputmode=\"numeric\"",
            "name=\"year\" type=\"number\"",
            "name=\"search\" inputmode=\"numeric\"",
        ] {
            assert!(fixture.contains(ordinary));
        }
    }
}
