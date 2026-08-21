//! Bounded semantic observation and action for Tidebreak-owned browser tabs.
//!
//! Page data is treated as untrusted throughout. The host retains selectors
//! and fingerprints, while agents receive only compact ephemeral refs. Every
//! action must present the exact snapshot id and document epoch that produced
//! its ref; navigation or a replaced target returns `stale_target` without an
//! action.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
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

use crate::{
    browser_control::{
        BrowserDispatchEffect, BrowserLoadState, BrowserRegistry, BrowserSnapshot,
        BrowserTargetError, BrowserTargetFingerprint, BrowserTargetRecord,
    },
    code_browser::browser_label,
};

const MAX_ACTION_VALUE_CHARS: usize = 8_192;
const JAVASCRIPT_TIMEOUT_SECONDS: u64 = 10;

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserSemanticActionRequest {
    browser_id: String,
    snapshot_id: String,
    document_epoch: u64,
    #[serde(rename = "ref")]
    target_ref: String,
    action: BrowserSemanticAction,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
enum BrowserSemanticAction {
    Click,
    Focus,
    Fill { value: String },
    Select { value: String },
    Check { checked: bool },
    Press { key: String },
    ScrollIntoView,
}

impl BrowserSemanticAction {
    fn kind(&self) -> &'static str {
        match self {
            Self::Click => "click",
            Self::Focus => "focus",
            Self::Fill { .. } => "fill",
            Self::Select { .. } => "select",
            Self::Check { .. } => "check",
            Self::Press { .. } => "press",
            Self::ScrollIntoView => "scroll_into_view",
        }
    }

    fn value(&self) -> Option<&str> {
        match self {
            Self::Fill { value } | Self::Select { value } => Some(value),
            Self::Press { key } => Some(key),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BrowserSemanticActionStatus {
    Ok,
    StaleTarget,
    HumanTakeoverRequired,
    BrowserHidden,
    Unsupported,
    InvalidValue,
    TargetObscured,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserSemanticActionResult {
    browser_id: String,
    snapshot_id: String,
    document_epoch: u64,
    #[serde(rename = "ref")]
    target_ref: String,
    action: String,
    status: BrowserSemanticActionStatus,
    message: String,
    requires_resnapshot: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawActionResult {
    status: BrowserSemanticActionStatus,
    message: String,
    url: Option<String>,
    title: Option<String>,
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
    let host_snapshot = registry.begin_agent_control(capability_id, &arguments.browser_id)?;
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
    workspace_id: String,
    browser_id: String,
    max_nodes: usize,
) -> Result<BrowserPageSnapshot, String> {
    let label = browser_label(&browser_id)?;
    let host_snapshot = registry.snapshot(&browser_id, &workspace_id)?;
    if host_snapshot.load_state != Some(BrowserLoadState::Ready) {
        return Err("browser page is still loading".to_owned());
    }
    let document_epoch = host_snapshot
        .document_epoch
        .ok_or_else(|| "browser document epoch is unavailable".to_owned())?;
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
    registry
        .record_semantic_snapshot(
            &browser_id,
            &workspace_id,
            document_epoch,
            snapshot_id.clone(),
            targets,
        )
        .map_err(|_| "browser document changed while it was being inspected".to_owned())?;

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

pub(crate) async fn browser_semantic_action(
    app: &AppHandle,
    registry: &BrowserRegistry,
    capability_id: Uuid,
    request: BrowserSemanticActionRequest,
) -> Result<BrowserSemanticActionResult, String> {
    if let Some(value) = request.action.value() {
        if value.chars().count() > MAX_ACTION_VALUE_CHARS {
            return Err("browser action value is too long".to_owned());
        }
    }

    let host_snapshot = registry.begin_agent_control(capability_id, &request.browser_id)?;
    if !host_snapshot
        .engine
        .as_ref()
        .is_some_and(|engine| engine.capabilities.semantic_actions)
    {
        return Ok(action_result(
            &request,
            BrowserSemanticActionStatus::Unsupported,
            "This browser can inspect semantic targets, but trusted native input is not available yet.",
        ));
    }
    let workspace_id = host_snapshot.workspace_id;
    let origin = host_snapshot
        .url
        .as_deref()
        .and_then(BrowserOrigin::from_url)
        .ok_or_else(|| "browser has no authorized HTTP origin".to_owned())?;
    let target = match registry.semantic_target(
        &request.browser_id,
        &workspace_id,
        &request.snapshot_id,
        request.document_epoch,
        &request.target_ref,
    ) {
        Ok(target) => target,
        Err(BrowserTargetError::StaleTarget) => {
            return Ok(action_result(
                &request,
                BrowserSemanticActionStatus::StaleTarget,
                "The page or target changed. Take a new snapshot before acting.",
            ));
        }
        Err(BrowserTargetError::BrowserHidden) => {
            return Ok(action_result(
                &request,
                BrowserSemanticActionStatus::BrowserHidden,
                "Bring this browser tab to the foreground before acting.",
            ));
        }
    };
    if target.sensitive {
        let _ = registry.set_agent_action(
            capability_id,
            &request.browser_id,
            Some("Waiting for human input"),
            true,
        );
        return Ok(action_result(
            &request,
            BrowserSemanticActionStatus::HumanTakeoverRequired,
            "Password and one-time-code fields require human takeover.",
        ));
    }

    let action_type = request.action.kind().to_owned();
    let target_label =
        (!target.fingerprint.name.is_empty()).then(|| target.fingerprint.name.clone());
    let consequential = target.consequential && !origin.is_loopback();
    let confirmation_id = if consequential {
        let _ = registry.set_agent_action(
            capability_id,
            &request.browser_id,
            Some("Waiting for native confirmation"),
            false,
        );
        if !native_consequential_action_choice(app, &origin, &action_type, target_label.as_deref())
            .await?
        {
            let _ = registry.set_agent_action(capability_id, &request.browser_id, None, false);
            return Ok(action_result(
                &request,
                BrowserSemanticActionStatus::HumanTakeoverRequired,
                "The user declined this consequential browser action. Do not retry it without direction.",
            ));
        }
        Some(registry.record_native_confirmation(
            capability_id,
            &request.browser_id,
            &origin,
            &action_type,
            target_label.as_deref(),
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
    let browser_id = request.browser_id.clone();
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
                execute_semantic_action(app, dispatch_registry, workspace_id, request).await
            },
        )
        .await
}

async fn execute_semantic_action(
    app: AppHandle,
    registry: BrowserRegistry,
    workspace_id: String,
    request: BrowserSemanticActionRequest,
) -> Result<BrowserSemanticActionResult, String> {
    let label = browser_label(&request.browser_id)?;
    let target = match registry.semantic_target(
        &request.browser_id,
        &workspace_id,
        &request.snapshot_id,
        request.document_epoch,
        &request.target_ref,
    ) {
        Ok(target) => target,
        Err(BrowserTargetError::StaleTarget) => {
            return Ok(action_result(
                &request,
                BrowserSemanticActionStatus::StaleTarget,
                "The page or target changed. Take a new snapshot before acting.",
            ));
        }
        Err(BrowserTargetError::BrowserHidden) => {
            return Ok(action_result(
                &request,
                BrowserSemanticActionStatus::BrowserHidden,
                "Bring this browser tab to the foreground before acting.",
            ));
        }
    };
    if target.sensitive {
        return Ok(action_result(
            &request,
            BrowserSemanticActionStatus::HumanTakeoverRequired,
            "Password and one-time-code fields require human takeover.",
        ));
    }
    let webview = app
        .get_webview(&label)
        .ok_or_else(|| "browser session is not open".to_owned())?;
    let raw: RawActionResult =
        evaluate_json(&webview, &action_script(&target, &request.action)?).await?;
    registry.invalidate_semantic_snapshot(&request.browser_id, &workspace_id, &request.snapshot_id);

    Ok(BrowserSemanticActionResult {
        browser_id: request.browser_id,
        snapshot_id: request.snapshot_id,
        document_epoch: request.document_epoch,
        target_ref: request.target_ref,
        action: request.action.kind().to_owned(),
        status: raw.status,
        message: raw.message,
        requires_resnapshot: true,
        url: raw.url,
        title: raw.title,
    })
}

fn action_result(
    request: &BrowserSemanticActionRequest,
    status: BrowserSemanticActionStatus,
    message: &str,
) -> BrowserSemanticActionResult {
    BrowserSemanticActionResult {
        browser_id: request.browser_id.clone(),
        snapshot_id: request.snapshot_id.clone(),
        document_epoch: request.document_epoch,
        target_ref: request.target_ref.clone(),
        action: request.action.kind().to_owned(),
        status,
        message: message.to_owned(),
        requires_resnapshot: matches!(status, BrowserSemanticActionStatus::StaleTarget),
        url: None,
        title: None,
    }
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
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.parent(&window);
    }
    dialog.show(move |approved| {
        let _ = sender.send(approved);
    });
    receiver
        .await
        .map_err(|_| "the native browser confirmation closed unexpectedly".to_owned())
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
    let host_snapshot = registry.begin_agent_control(capability_id, &arguments.browser_id)?;
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
                poll_wait_condition(app, dispatch_registry, capability_id, workspace_id, arguments)
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
    let mut halt = registry.subscribe_halt(&arguments.browser_id, &workspace_id)?;

    // Capture the starting URL for UrlChanged condition
    let start_snapshot = registry.snapshot(&arguments.browser_id, &workspace_id)?;
    let start_url = start_snapshot.url.clone();

    loop {
        let snapshot = registry.snapshot(&arguments.browser_id, &workspace_id)?;
        let Some(document_epoch) = snapshot.document_epoch else {
            return Ok(wait_result(
                &arguments.browser_id,
                &snapshot,
                BrowserWaitStatus::TimedOut,
                "browser document epoch is unavailable".to_owned(),
            ));
        };

        if *halt.borrow_and_update()
            || snapshot.agent_access.as_ref().is_some_and(|access| access.halted)
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
            satisfied = evaluate_wait_condition(
                &webview,
                &arguments.condition,
                &snapshot,
                start_url.as_deref(),
            ) => satisfied,
        };

        if satisfied {
            // Completion-time fencing: report Resolved only while the
            // capability, grant, controller, visibility, and instance that
            // authorized this wait are all still live.
            let final_snapshot = registry.snapshot(&arguments.browser_id, &workspace_id)?;
            if *halt.borrow_and_update()
                || final_snapshot.agent_access.as_ref().is_some_and(|access| access.halted)
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
            // condition was probed against `document_epoch`; if the document
            // changed while the probe was in flight, discard the stale
            // result and re-evaluate against the new document.
            if fence.document_epoch == document_epoch
                || matches!(arguments.condition, BrowserWaitCondition::UrlChanged)
            {
                return Ok(wait_result(
                    &arguments.browser_id,
                    &final_snapshot,
                    BrowserWaitStatus::Resolved,
                    "Wait condition satisfied.".to_owned(),
                ));
            }
            continue;
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

async fn evaluate_wait_condition(
    webview: &Webview,
    condition: &tidebreak_core::BrowserWaitCondition,
    snapshot: &BrowserSnapshot,
    start_url: Option<&str>,
) -> bool {
    use tidebreak_core::BrowserWaitCondition;

    match condition {
        BrowserWaitCondition::LoadState { state } => snapshot.load_state == Some(*state),
        BrowserWaitCondition::UrlChanged => snapshot.url.as_deref() != start_url,
        BrowserWaitCondition::TextPresent { text } => {
            page_contains_text(webview, text).await.unwrap_or(false)
        }
        BrowserWaitCondition::TextAbsent { text } => {
            !page_contains_text(webview, text).await.unwrap_or(true)
        }
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
        let safe_text = text.replace('\\', "\\\\").replace('\'', "\\'");
        let script = format!(
            "JSON.stringify(Boolean(document.body && document.body.innerText.indexOf('{safe_text}') !== -1))"
        );
        let raw: String = evaluate_json(webview, &script).await?;
        Ok(raw == "true")
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
    let host_snapshot = registry.begin_agent_control(capability_id, &arguments.browser_id)?;
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
                capture_screenshot(app, dispatch_registry, capability_id, workspace_id, arguments)
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

    // The screenshot is bound to the live stored semantic snapshot; the
    // snapshot id echoed back to the model is never trusted from the
    // request alone.
    registry.validate_snapshot_id(
        &arguments.browser_id,
        &workspace_id,
        &arguments.snapshot_id,
        arguments.document_epoch,
    )?;

    let start_fence = registry.observation_fence(capability_id, &arguments.browser_id)?;
    let document_epoch = start_fence.document_epoch;
    if document_epoch != arguments.document_epoch {
        return Err("browser document changed since the snapshot was taken".to_owned());
    }

    let webview = app
        .get_webview(&label)
        .ok_or_else(|| "browser session is not open".to_owned())?;

    let image_base64 =
        capture_browser_image(&webview, arguments.max_width, arguments.max_height).await?;

    // Completion-time fencing: discard the image unless the capability,
    // grant, controller, visibility, instance, epoch, and stored snapshot
    // are all still live after the async capture.
    let end_fence = registry.observation_fence(capability_id, &arguments.browser_id)?;
    if end_fence.instance_id != start_fence.instance_id
        || end_fence.document_epoch != document_epoch
    {
        return Err("browser document changed while screenshot was being captured".to_owned());
    }
    registry.validate_snapshot_id(
        &arguments.browser_id,
        &workspace_id,
        &arguments.snapshot_id,
        document_epoch,
    )?;
    registry
        .record_screenshot_epoch(&arguments.browser_id, &workspace_id, document_epoch)
        .map_err(|_| "browser document changed while screenshot was being captured".to_owned())?;

    Ok(BrowserScreenshotResult {
        browser_id: arguments.browser_id,
        snapshot_id: arguments.snapshot_id,
        document_epoch,
        image_base64,
        mime_type: "image/png".to_owned(),
    })
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

    webview
        .with_webview(move |platform| unsafe {
            let view: &WKWebView = &*platform.inner().cast();
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
        })
        .map_err(|error| format!("browser host: {error}"))?;

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
    _app: &AppHandle,
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

    let host_snapshot = registry.begin_agent_control(capability_id, &arguments.browser_id)?;
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

    // Until the native trusted-input adapter is wired, every action returns
    // UnsupportedNative. This is the correct default: we must never advertise
    // semantic_actions=true while the adapter still dispatches page-authored
    // DOM events.
    let _ = registry.set_agent_action(
        capability_id,
        &arguments.browser_id,
        Some("Trusted native input unavailable"),
        false,
    );
    Ok(act_result(
        &arguments,
        BrowserActStatus::UnsupportedNative,
        "The browser semantic driver can observe and wait, but trusted native input is not available on this engine.",
    ))
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
}

fn action_script(
    target: &BrowserTargetRecord,
    action: &BrowserSemanticAction,
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
        "action": match action {
            BrowserSemanticAction::Click => serde_json::json!({ "type": "click" }),
            BrowserSemanticAction::Focus => serde_json::json!({ "type": "focus" }),
            BrowserSemanticAction::Fill { value } => {
                serde_json::json!({ "type": "fill", "value": value })
            }
            BrowserSemanticAction::Select { value } => {
                serde_json::json!({ "type": "select", "value": value })
            }
            BrowserSemanticAction::Check { checked } => {
                serde_json::json!({ "type": "check", "checked": checked })
            }
            BrowserSemanticAction::Press { key } => {
                serde_json::json!({ "type": "press", "key": key })
            }
            BrowserSemanticAction::ScrollIntoView => {
                serde_json::json!({ "type": "scroll_into_view" })
            }
        },
    });
    let payload = serde_json::to_string(&payload).map_err(|error| error.to_string())?;
    Ok(ACTION_SCRIPT.replace("__PAYLOAD__", &payload))
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
    use objc2_web_kit::WKWebView;
    use tokio::{sync::oneshot, time::timeout};

    let (sender, receiver) = oneshot::channel();
    let sender = Mutex::new(Some(sender));
    let script = script.to_owned();
    webview
        .with_webview(move |platform| unsafe {
            let view: &WKWebView = &*platform.inner().cast();
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
            view.evaluateJavaScript_completionHandler(&NSString::from_str(&script), Some(&handler));
        })
        .map_err(|error| format!("browser host: {error}"))?;

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

const SNAPSHOT_SCRIPT: &str = r#"
(() => {
  const MAX_NODES = __MAX_NODES__;
  const MARKER = "__MARKER__";
  const TEXT_LIMIT = 240;
  const nodes = [];
  const frames = [];
  let truncated = false;
  let nextTargetRef = 1;

  const INTERACTIVE_SELECTOR = [
    "a[href]", "button", "input:not([type='hidden'])", "textarea", "select",
    "[contenteditable='true']", "[role='button']", "[role='link']",
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
  const accessibleName = (element, doc) => {
    const labelledBy = clean(element.getAttribute("aria-labelledby"), 200);
    if (labelledBy) {
      const value = labelledBy.split(/\s+/)
        .map((id) => clean(doc.getElementById(id)?.textContent))
        .filter(Boolean)
        .join(" ");
      if (value) return clean(value);
    }
    const direct = clean(element.getAttribute("aria-label"))
      || clean(element.labels && Array.from(element.labels).map((label) => label.textContent).join(" "))
      || clean(element.getAttribute("alt"))
      || clean(element.getAttribute("title"))
      || clean(element.getAttribute("placeholder"));
    if (direct) return direct;
    return clean(element.innerText || element.textContent);
  };
  const isSensitive = (element) => {
    if (element.localName !== "input") return false;
    const type = clean(element.getAttribute("type"), 40).toLowerCase();
    const autocomplete = clean(element.getAttribute("autocomplete"), 100).toLowerCase();
    return type === "password"
      || type === "file"
      || autocomplete.includes("one-time-code")
      || autocomplete.includes("current-password")
      || autocomplete.includes("new-password");
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
    if (sensitive) return ["human_takeover"];
    const actions = ["focus", "scroll_into_view"];
    if (["button", "link", "checkbox", "radio", "tab"].includes(role)) actions.unshift("click");
    if (role === "textbox") actions.unshift("fill");
    if (role === "combobox") actions.unshift("select");
    if (role === "checkbox" || role === "radio") actions.unshift("check");
    actions.push("press");
    return Array.from(new Set(actions));
  };
  const contentText = (element) => element.localName === "img"
    ? clean(element.getAttribute("alt"))
    : clean(element.innerText || element.textContent);

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
      const text = contentText(element);
      if (!interactive && !text) continue;
      const rect = element.getBoundingClientRect();
      const role = inferredRole(element);
      const sensitive = interactive && isSensitive(element);
      const consequential = interactive && isConsequential(element);
      const targetRef = interactive ? `@e${nextTargetRef++}` : null;
      if (interactive) {
        try {
          Object.defineProperty(element, MARKER, { value: targetRef, configurable: true });
        } catch (_) {
          try { element[MARKER] = targetRef; } catch (_) { continue; }
        }
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
        name: interactive ? accessibleName(element, doc) : text,
        frame: frameName,
        text: text || null,
        value: value || null,
        href: interactive && element.href ? clean(element.href, 2048) : null,
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

const ACTION_SCRIPT: &str = r#"
(() => {
  const payload = __PAYLOAD__;
  const clean = (value, limit = 240) => String(value || "")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, limit);
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
  const nameFor = (element, doc) => {
    const labelledBy = clean(element.getAttribute("aria-labelledby"), 200);
    if (labelledBy) {
      const value = labelledBy.split(/\s+/)
        .map((id) => clean(doc.getElementById(id)?.textContent))
        .filter(Boolean)
        .join(" ");
      if (value) return clean(value);
    }
    return clean(element.getAttribute("aria-label"))
      || clean(element.labels && Array.from(element.labels).map((label) => label.textContent).join(" "))
      || clean(element.getAttribute("alt"))
      || clean(element.getAttribute("title"))
      || clean(element.getAttribute("placeholder"))
      || clean(element.innerText || element.textContent);
  };
  const result = (status, message) => JSON.stringify({
    status,
    message,
    url: String(location.href),
    title: clean(document.title, 160),
  });

  let doc = document;
  for (const selector of payload.framePath) {
    const frame = doc.querySelector(selector);
    if (!frame) return result("stale_target", "The target frame changed.");
    try {
      doc = frame.contentDocument;
    } catch (_) {
      return result("unsupported", "The target is inside a cross-origin frame.");
    }
    if (!doc) return result("stale_target", "The target frame is unavailable.");
  }
  const element = doc.querySelector(payload.selector);
  if (!element || !element.isConnected) return result("stale_target", "The target no longer exists.");
  if (element[payload.marker] !== payload.markerValue) {
    return result("stale_target", "The target element was replaced.");
  }
  const isSensitive = (() => {
    if (element.localName !== "input") return false;
    const type = clean(element.getAttribute("type"), 40).toLowerCase();
    const autocomplete = clean(element.getAttribute("autocomplete"), 100).toLowerCase();
    return type === "password"
      || type === "file"
      || autocomplete.includes("one-time-code")
      || autocomplete.includes("current-password")
      || autocomplete.includes("new-password");
  })();
  const fresh = {
    tag: element.localName || "element",
    role: roleFor(element),
    name: nameFor(element, doc),
    inputType: element.localName === "input"
      ? clean(element.getAttribute("type") || "text", 40).toLowerCase()
      : null,
    href: element.href ? clean(element.href, 2048) : null,
    sensitive: isSensitive,
  };
  if (JSON.stringify(fresh) !== JSON.stringify(payload.fingerprint)) {
    return result("stale_target", "The target's identifying content changed.");
  }
  if (element.disabled || element.getAttribute("aria-disabled") === "true") {
    return result("invalid_value", "The target is disabled.");
  }

  const action = payload.action;
  if (action.type === "click") {
    const rect = element.getBoundingClientRect();
    const hit = doc.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2);
    if (hit && hit !== element && !element.contains(hit)) {
      return result("target_obscured", "Another element is covering the target.");
    }
    element.focus({ preventScroll: true });
    element.click();
  } else if (action.type === "focus") {
    element.focus({ preventScroll: true });
  } else if (action.type === "fill") {
    const view = doc.defaultView;
    if (!view) return result("stale_target", "The target document is unavailable.");
    if (!(element instanceof view.HTMLInputElement)
      && !(element instanceof view.HTMLTextAreaElement)
      && !element.isContentEditable) {
      return result("unsupported", "This target cannot be filled.");
    }
    element.focus({ preventScroll: true });
    if (element.isContentEditable) {
      element.textContent = action.value;
    } else {
      const prototype = element instanceof view.HTMLTextAreaElement
        ? view.HTMLTextAreaElement.prototype
        : view.HTMLInputElement.prototype;
      const setter = Object.getOwnPropertyDescriptor(prototype, "value")?.set;
      if (!setter) return result("unsupported", "This input does not expose a value setter.");
      setter.call(element, action.value);
    }
    element.dispatchEvent(new view.InputEvent("input", { bubbles: true, composed: true, inputType: "insertText", data: action.value }));
    element.dispatchEvent(new view.Event("change", { bubbles: true, composed: true }));
  } else if (action.type === "select") {
    const view = doc.defaultView;
    if (!view || !(element instanceof view.HTMLSelectElement)) return result("unsupported", "This target is not a select control.");
    if (!Array.from(element.options).some((option) => option.value === action.value)) {
      return result("invalid_value", "The requested option is not present.");
    }
    element.value = action.value;
    element.dispatchEvent(new view.Event("input", { bubbles: true, composed: true }));
    element.dispatchEvent(new view.Event("change", { bubbles: true, composed: true }));
  } else if (action.type === "check") {
    const view = doc.defaultView;
    if (!view || !(element instanceof view.HTMLInputElement) || !["checkbox", "radio"].includes(element.type)) {
      return result("unsupported", "This target is not a checkbox or radio control.");
    }
    element.checked = Boolean(action.checked);
    element.dispatchEvent(new view.Event("input", { bubbles: true, composed: true }));
    element.dispatchEvent(new view.Event("change", { bubbles: true, composed: true }));
  } else if (action.type === "press") {
    const view = doc.defaultView;
    if (!view) return result("stale_target", "The target document is unavailable.");
    const supported = ["Enter", "Escape", "Tab", " ", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Backspace", "Delete"];
    if (!supported.includes(action.key)) return result("unsupported", "This key is not supported by the semantic driver.");
    element.focus({ preventScroll: true });
    element.dispatchEvent(new view.KeyboardEvent("keydown", { key: action.key, bubbles: true, composed: true, cancelable: true }));
    if (action.key === "Enter") {
      if (element instanceof view.HTMLButtonElement || element.getAttribute("role") === "button") element.click();
      else if (element.form) element.form.requestSubmit();
    } else if (action.key === " ") {
      if (element instanceof view.HTMLButtonElement || ["checkbox", "radio"].includes(element.type)) element.click();
    }
    element.dispatchEvent(new view.KeyboardEvent("keyup", { key: action.key, bubbles: true, composed: true }));
  } else if (action.type === "scroll_into_view") {
    element.scrollIntoView({ block: "center", inline: "center", behavior: "instant" });
  } else {
    return result("unsupported", "This semantic action is not supported.");
  }

  return result("ok", "Action completed. Take a new snapshot before the next action.");
})()
"#;

#[cfg(test)]
mod tests {
    use super::*;

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
        let script = action_script(
            &target,
            &BrowserSemanticAction::Fill {
                value: "'); globalThis.pwned = true; ('".to_owned(),
            },
        )
        .unwrap();
        assert!(script.contains("globalThis.pwned"));
        assert!(script.contains("const payload = {"));
        assert!(!script.contains("value: '); globalThis.pwned"));
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
    }
}
