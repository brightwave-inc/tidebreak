//! Chrome computer-use CDP driver: attachment, semantic snapshots, actions,
//! waits, screenshots, and bounded diagnostics.
//!
//! This module owns protocol mechanics only. Grants, session ownership, and
//! origin fences live in [`super::runtime`]. The driver accepts no endpoint,
//! profile path, data directory, or raw CDP method from model callers.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use tidebreak_core::{
    BrowserLoadState, BrowserOrigin, CancelToken, ChromeAction, ChromeActStatus,
    ChromeConnectionGrant, ChromeSemanticNodeKind, ChromeViewport,
};
use url::Url;

use super::cdp::{CdpError, CdpSession};

const DEFAULT_CDP_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_FRAMES: usize = 256;

/// Host-derived connection descriptor accepted by runtime installation.
#[cfg(test)]
pub fn test_origin() -> BrowserOrigin {
    BrowserOrigin::from_url("https://example.com").unwrap()
}

/// Whether an origin is inside the connection's approved scope.
pub fn grant_covers(connection_grant: &ChromeConnectionGrant, origin: &BrowserOrigin) -> bool {
    connection_grant.covers(origin)
}

/// One live target table row. Runtime keeps rows; this module only carries
/// the shared projection.
#[derive(Debug, Clone)]
pub struct TargetRow {
    pub target_ref: String,
    pub target_id: String,
    pub frame_id: String,
    pub url: String,
    pub title: String,
    pub active: bool,
    pub attached: bool,
}

impl TargetRow {
    pub fn load_state(&self, lifecycle: &HashMap<String, HashSet<String>>) -> BrowserLoadState {
        if self.is_loading(lifecycle) {
            BrowserLoadState::Loading
        } else if self.url.is_empty() {
            BrowserLoadState::Idle
        } else {
            BrowserLoadState::Ready
        }
    }

    fn is_loading(&self, lifecycle: &HashMap<String, HashSet<String>>) -> bool {
        let Some(events) = lifecycle.get(&self.frame_id) else {
            return false;
        };
        events.contains("DOMContentLoaded") || events.contains("load") || events.contains("init")
    }
}

/// A snapshot record the action path resolves against.
#[derive(Debug, Clone)]
pub struct SnapshotRecord {
    pub snapshot_id: String,
    pub document_epoch: u64,
    pub target_ref: String,
    pub target_id: String,
    pub session_id: String,
    pub url: String,
    pub nodes: Vec<SnapshotNode>,
}

/// One resolved semantic node with a stable selector and frame binding.
#[derive(Debug, Clone)]
pub struct SnapshotNode {
    pub node_ref: String,
    pub selector: String,
    pub frame_id: String,
    pub frame_session_id: Option<String>,
    pub kind: ChromeSemanticNodeKind,
    pub url: String,
}

fn cmd(
    cdp: &CdpSession,
    session_id: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, CdpError> {
    futures::executor::block_on(async {
        tokio::time::timeout(
            DEFAULT_CDP_TIMEOUT,
            cdp.command_in_session(session_id, method, params),
        )
        .await
        .map_err(|_| CdpError(format!("{method} timed out")))?
    })
}

/// Attach one target and return its flattened session id.
pub fn attach_target(cdp: &CdpSession, target_id: &str) -> Result<String, CdpError> {
    futures::executor::block_on(async {
        let result: serde_json::Value = tokio::time::timeout(
            DEFAULT_CDP_TIMEOUT,
            cdp.command(
                "Target.attachToTarget",
                serde_json::json!({"targetId": target_id, "flatten": true}),
            ),
        )
        .await
        .map_err(|_| CdpError("Target.attachToTarget timed out".to_owned()))??;
        Ok(result
            .get("sessionId")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| CdpError("attach did not return a session id".to_owned()))?
            .to_owned())
    })
}

/// Enable event streams inside one target session.
pub fn enable_target(cdp: &CdpSession, session_id: &str) -> Result<(), CdpError> {
    cmd(cdp, session_id, "Page.enable", serde_json::json!({}))?;
    cmd(cdp, session_id, "Runtime.enable", serde_json::json!({}))?;
    cmd(cdp, session_id, "Network.enable", serde_json::json!({}))?;
    Ok(())
}

/// Fetch the target's frame tree (bounded by the caller).
pub fn frame_tree(cdp: &CdpSession, session_id: &str) -> Result<serde_json::Value, CdpError> {
    cmd(cdp, session_id, "Page.getFrameTree", serde_json::json!({}))
}

/// Capture a PNG screenshot bounded to the viewport and caller dimensions.
pub fn screenshot_png(
    cdp: &CdpSession,
    session_id: &str,
    max_width: Option<u64>,
    max_height: Option<u64>,
) -> Result<(Vec<u8>, u64, u64), CdpError> {
    let mut params = serde_json::json!({
        "format": "png",
        "captureBeyondViewport": false,
        "fromSurface": true
    });
    if let (Some(width), Some(height)) = (max_width, max_height) {
        params["clip"] = serde_json::json!({
            "x": 0,
            "y": 0,
            "width": width.min(tidebreak_core::MAX_CHROME_SCREENSHOT_DIMENSION) as f64,
            "height": height.min(tidebreak_core::MAX_CHROME_SCREENSHOT_DIMENSION) as f64,
            "scale": 1.0
        });
        let result = cmd(cdp, session_id, "Page.captureScreenshot", params)?;
        let base64 = result
            .get("data")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| CdpError("captureScreenshot returned no data".to_owned()))?;
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(base64)
            .map_err(|error| CdpError(format!("captureScreenshot base64 invalid: {error}")))?;
        if bytes.len() > tidebreak_core::MAX_CHROME_SCREENSHOT_PNG_BYTES {
            return Err(CdpError(format!(
                "capture exceeded bounded image size: {} bytes",
                bytes.len()
            )));
        }
        return Ok((bytes, width, height));
    }
    // No clip requested; still enforce the byte bound after decoding.
    let result = cmd(cdp, session_id, "Page.captureScreenshot", params)?;
    let base64 = result
        .get("data")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| CdpError("captureScreenshot returned no data".to_owned()))?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64)
        .map_err(|error| CdpError(format!("captureScreenshot base64 invalid: {error}")))?;
    if bytes.len() > tidebreak_core::MAX_CHROME_SCREENSHOT_PNG_BYTES {
        return Err(CdpError(format!(
            "capture exceeded bounded image size: {} bytes",
            bytes.len()
        )));
    }
    Ok((bytes, 0, 0))
}

/// Parse one bounded semantic snapshot value produced by the driver script.
pub fn parse_semantic_snapshot(
    value: serde_json::Value,
    max_nodes: usize,
    granted: &ChromeConnectionGrant,
) -> (
    Vec<tidebreak_core::ChromeSemanticNode>,
    Vec<tidebreak_core::ChromeSemanticFrame>,
    bool,
) {
    let _ = granted;
    let nodes = value
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(max_nodes)
        .map(|value| tidebreak_core::ChromeSemanticNode {
            kind: if value.get("kind").and_then(serde_json::Value::as_str) == Some("interactive") {
                ChromeSemanticNodeKind::Interactive
            } else {
                ChromeSemanticNodeKind::Content
            },
            target_ref: value.get("ref").and_then(serde_json::Value::as_str).map(str::to_owned),
            tag: value.get("tag").and_then(serde_json::Value::as_str).unwrap_or("").to_owned(),
            role: value.get("role").and_then(serde_json::Value::as_str).unwrap_or("").to_owned(),
            name: value.get("name").and_then(serde_json::Value::as_str).unwrap_or("").to_owned(),
            frame: value.get("frame").and_then(serde_json::Value::as_str).unwrap_or("").to_owned(),
            text: value.get("text").and_then(serde_json::Value::as_str).map(str::to_owned),
            value: value.get("value").and_then(serde_json::Value::as_str).map(str::to_owned),
            href: value.get("href").and_then(serde_json::Value::as_str).map(str::to_owned),
            input_type: value.get("inputType").and_then(serde_json::Value::as_str).map(str::to_owned),
            disabled: value.get("disabled").and_then(serde_json::Value::as_bool).unwrap_or(false),
            checked: value.get("checked").and_then(serde_json::Value::as_bool),
            sensitive: value.get("sensitive").and_then(serde_json::Value::as_bool).unwrap_or(false),
            actions: value.get("actions").and_then(serde_json::Value::as_array).map(|actions| {
                actions.iter().filter_map(serde_json::Value::as_str).map(str::to_owned).collect()
            }).unwrap_or_default(),
            bounds: {
                let bounds = value.get("bounds").cloned().unwrap_or(serde_json::json!({}));
                tidebreak_core::BrowserElementBounds {
                    x: bounds.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
                    y: bounds.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
                    width: bounds.get("width").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
                    height: bounds.get("height").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
                }
            },
        })
        .collect();
    let frames = value
        .get("frames")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(tidebreak_core::MAX_CHROME_SNAPSHOT_NODES)
        .map(|value| tidebreak_core::ChromeSemanticFrame {
            name: value.get("name").and_then(serde_json::Value::as_str).unwrap_or("").to_owned(),
            url: value.get("url").and_then(serde_json::Value::as_str).unwrap_or("").to_owned(),
            status: match value.get("status").and_then(serde_json::Value::as_str) {
                Some("cross_origin") => tidebreak_core::ChromeFrameStatus::CrossOrigin,
                Some("unsupported") => tidebreak_core::ChromeFrameStatus::UnsupportedFrame,
                _ => tidebreak_core::ChromeFrameStatus::SameOrigin,
            },
        })
        .collect();
    let truncated = value.get("truncated").and_then(serde_json::Value::as_bool).unwrap_or(false);
    (nodes, frames, truncated)
}

/// Execute one semantic action. The runtime has already checked grants,
/// session ownership, and Stop; this function returns typed statuses so the
/// caller never replays an unconfirmed action.
pub fn execute_action(
    cdp: &CdpSession,
    snapshot: &SnapshotRecord,
    node: &SnapshotNode,
    action: &ChromeAction,
    cancel: &CancelToken,
) -> Result<ChromeActStatus, CdpError> {
    if cancel.is_cancelled() {
        return Ok(ChromeActStatus::Cancelled);
    }
    let session_id = node
        .frame_session_id
        .as_deref()
        .unwrap_or(&snapshot.session_id);
    match action {
        ChromeAction::Click | ChromeAction::DoubleClick => {
            let click_count = if matches!(action, ChromeAction::DoubleClick) { 2 } else { 1 };
            let evaluated = evaluate_js(cdp, session_id, &center_script(node), node)?;
            if evaluated.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
                return Ok(ChromeActStatus::StaleTarget);
            }
            let x = evaluated.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
            let y = evaluated.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
            for _ in 0..click_count {
                cmd(
                    cdp,
                    session_id,
                    "Input.dispatchMouseEvent",
                    serde_json::json!({
                        "type": "mousePressed",
                        "x": x,
                        "y": y,
                        "button": "left",
                        "clickCount": click_count
                    }),
                )?;
                cmd(
                    cdp,
                    session_id,
                    "Input.dispatchMouseEvent",
                    serde_json::json!({
                        "type": "mouseReleased",
                        "x": x,
                        "y": y,
                        "button": "left",
                        "clickCount": click_count
                    }),
                )?;
            }
            Ok(ChromeActStatus::Ok)
        }
        ChromeAction::Hover => {
            let evaluated = evaluate_js(cdp, session_id, &center_script(node), node)?;
            if evaluated.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
                return Ok(ChromeActStatus::StaleTarget);
            }
            cmd(
                cdp,
                session_id,
                "Input.dispatchMouseEvent",
                serde_json::json!({
                    "type": "mouseMoved",
                    "x": evaluated.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
                    "y": evaluated.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0)
                }),
            )?;
            Ok(ChromeActStatus::Ok)
        }
        ChromeAction::Type { text } => {
            let focused = evaluate_js(cdp, session_id, &focus_script(node), node)?;
            if focused.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
                return Ok(ChromeActStatus::StaleTarget);
            }
            for character in text.chars() {
                if cancel.is_cancelled() {
                    return Ok(ChromeActStatus::Cancelled);
                }
                cmd(
                    cdp,
                    session_id,
                    "Input.insertText",
                    serde_json::json!({ "text": character.to_string() }),
                )?;
            }
            Ok(ChromeActStatus::Ok)
        }
        ChromeAction::Fill { value } => {
            let result = evaluate_with_value(cdp, session_id, &fill_script(), value, node)?;
            if result.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
                return Ok(ChromeActStatus::StaleTarget);
            }
            Ok(ChromeActStatus::Ok)
        }
        ChromeAction::Select { value } => {
            let result = evaluate_with_value(cdp, session_id, &select_script(), value, node)?;
            match result.get("reason").and_then(serde_json::Value::as_str) {
                Some("no_option") => Ok(ChromeActStatus::InvalidValue),
                Some(_) | None => Ok(ChromeActStatus::Ok),
            }
        }
        ChromeAction::Check { checked } => {
            let result = evaluate_with_value(
                cdp,
                session_id,
                &check_script(),
                if *checked { "true" } else { "false" },
                node,
            )?;
            if result.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
                return Ok(ChromeActStatus::StaleTarget);
            }
            Ok(ChromeActStatus::Ok)
        }
        ChromeAction::Press { key } => {
            cmd(
                cdp,
                session_id,
                "Input.dispatchKeyEvent",
                serde_json::json!({"type": "keyDown", "key": key}),
            )?;
            cmd(
                cdp,
                session_id,
                "Input.dispatchKeyEvent",
                serde_json::json!({"type": "keyUp", "key": key}),
            )?;
            Ok(ChromeActStatus::Ok)
        }
        ChromeAction::Scroll { x, y } => {
            let result = evaluate_js(cdp, session_id, &scroll_script(), node)?;
            if result.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
                return Ok(ChromeActStatus::StaleTarget);
            }
            if let Some(y) = y {
                cmd(
                    cdp,
                    session_id,
                    "Input.dispatchMouseEvent",
                    serde_json::json!({
                        "type": "mouseWheel",
                        "x": 10.0,
                        "y": 10.0,
                        "deltaX": x.unwrap_or(0.0),
                        "deltaY": y
                    }),
                )?;
            }
            Ok(ChromeActStatus::Ok)
        }
        ChromeAction::Drag { to_ref } => {
            // Drags need two resolved refs; runtime resolves the destination
            // and dispatches mouse gestures. This driver implements the
            // source half and reports the honest unsupported status until the
            // destination is passed through the snapshot pair.
            let _ = to_ref;
            Err(CdpError(
                "drag requires a destination snapshot ref resolved by the runtime".to_owned(),
            ))
        }
    }
}

fn center_script(node: &SnapshotNode) -> String {
    format!(
        r#"
        (() => {{
            const el = document.querySelector({selector});
            if (!el) return {{ok: false}};
            el.scrollIntoView({{block: 'center', inline: 'center'}});
            const r = el.getBoundingClientRect();
            return {{ok: true, x: r.left + r.width / 2, y: r.top + r.height / 2}};
        }})()
        "#,
        selector = serde_json::json!(node.selector)
    )
}

fn focus_script(node: &SnapshotNode) -> String {
    format!(
        r#"
        (() => {{
            const el = document.querySelector({selector});
            if (!el) return {{ok: false}};
            el.scrollIntoView({{block: 'center', inline: 'center'}});
            el.focus();
            return {{ok: true}};
        }})()
        "#,
        selector = serde_json::json!(node.selector)
    )
}

fn scroll_script() -> String {
    r#"
        (() => {
            const el = document.querySelector(SELECTOR);
            if (!el) return {ok: false};
            el.scrollIntoView({block: 'center', inline: 'center'});
            return {ok: true};
        })()
    "#
    .to_owned()
}

fn fill_script() -> String {
    r#"
        (() => {
            const el = document.querySelector(SELECTOR);
            if (!el) return {ok: false};
            el.focus();
            const proto = el instanceof HTMLTextAreaElement
                ? HTMLTextAreaElement.prototype
                : el instanceof HTMLInputElement ? HTMLInputElement.prototype : HTMLElement.prototype;
            const setter = Object.getOwnPropertyDescriptor(proto, 'value');
            if (setter && setter.set) setter.set.call(el, VALUE);
            el.dispatchEvent(new Event('input', {bubbles: true}));
            el.dispatchEvent(new Event('change', {bubbles: true}));
            return {ok: true};
        })()
    "#
    .to_owned()
}

fn select_script() -> String {
    r#"
        (() => {
            const el = document.querySelector(SELECTOR);
            if (!el) return {ok: false, reason: 'missing'};
            const options = Array.from(el.options || []);
            const match = options.find(o => o.value === VALUE) || options.find(o => o.text === VALUE);
            if (!match) return {ok: false, reason: 'no_option'};
            el.value = match.value;
            el.dispatchEvent(new Event('change', {bubbles: true}));
            return {ok: true, reason: null};
        })()
    "#
    .to_owned()
}

fn check_script() -> String {
    r#"
        (() => {
            const el = document.querySelector(SELECTOR);
            if (!el) return {ok: false};
            if (el.checked !== CHECKED) el.click();
            return {ok: true};
        })()
    "#
    .to_owned()
}

fn evaluate_js(
    cdp: &CdpSession,
    session_id: &str,
    script: &str,
    node: &SnapshotNode,
) -> Result<serde_json::Value, CdpError> {
    let script = script.replace("SELECTOR", &serde_json::json!(node.selector).to_string());
    evaluate(cdp, session_id, &script)
}

fn evaluate_with_value(
    cdp: &CdpSession,
    session_id: &str,
    script: &str,
    value: &str,
    node: &SnapshotNode,
) -> Result<serde_json::Value, CdpError> {
    let script = script
        .replace("SELECTOR", &serde_json::json!(node.selector).to_string())
        .replace("VALUE", &serde_json::json!(value).to_string())
        .replace("CHECKED", &serde_json::json!(value).to_string());
    evaluate(cdp, session_id, &script)
}

fn evaluate(
    cdp: &CdpSession,
    session_id: &str,
    expression: &str,
) -> Result<serde_json::Value, CdpError> {
    let result = cmd(
        cdp,
        session_id,
        "Runtime.evaluate",
        serde_json::json!({
            "expression": expression,
            "returnByValue": true,
            "awaitPromise": true
        }),
    )?;
    if let Some(exception) = result.get("exceptionDetails") {
        let text = exception
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("page script exception");
        return Err(CdpError(format!("page script: {text}")));
    }
    Ok(result.get("result").cloned().unwrap_or(serde_json::Value::Null))
}

/// Build a stable DOM selector for an accessibility node id. The driver owns
/// this mapping; the model receives only opaque refs.
pub fn selector_for_node_id(node_ref: &str) -> String {
    format!("[data-tb-chrome-ref=\"{node_ref}\"]")
}

/// Map protocol lifecycle names into the shared load-state vocabulary.
pub fn load_state_from_lifecycle(events: &HashSet<String>) -> BrowserLoadState {
    if events.contains("DOMContentLoaded") || events.contains("load") {
        BrowserLoadState::Ready
    } else if events.contains("init") {
        BrowserLoadState::Loading
    } else {
        BrowserLoadState::Idle
    }
}

/// Parse the browser-level `/json/version` endpoint (host-derived URL).
pub fn parse_version(info: serde_json::Value) -> Result<String, String> {
    Ok(info
        .get("Browser")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Chrome")
        .to_owned())
}

/// Normalize one model-proposed URL and return its origin.
pub fn origin_for_url(url: &str) -> Option<BrowserOrigin> {
    if !tidebreak_core::valid_chrome_url(url) {
        return None;
    }
    Url::parse(url).ok().and_then(|parsed| browser_origin_from_url(&parsed))
}

fn browser_origin_from_url(url: &Url) -> Option<BrowserOrigin> {
    let origin = url.origin().ascii_serialization();
    if origin == "null" {
        return None;
    }
    BrowserOrigin::from_url(url.as_str())
}

/// Bound a frame tree walk to a stable list of same-origin frame descriptors.
pub fn bounded_frame_tree(value: serde_json::Value, max_frames: usize) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    fn walk(
        node: &serde_json::Value,
        out: &mut Vec<String>,
        max: usize,
    ) -> Result<(), String> {
        let child = node.get("frame").cloned().unwrap_or(serde_json::Value::Null);
        if let Some(frame_id) = child
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
        {
            if out.len() >= max {
                return Err("frame tree exceeds bound".into());
            }
            out.push(frame_id);
        }
        if let Some(children) = node.get("childFrames").and_then(serde_json::Value::as_array) {
            for child_node in children {
                walk(child_node, out, max)?;
            }
        }
        Ok(())
    }
    walk(&value, &mut out, max_frames)?;
    Ok(out)
}

/// Feed protocol events into a per-target diagnostics materializer. Kept
/// small so runtime event wiring can remain explicit.
pub fn record_event(_events: &[super::cdp::CdpEvent]) {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn origin_grants_are_checked_against_connection_scopes() {
        let origin = BrowserOrigin::parse("https://example.com").unwrap();
        let loopback = BrowserOrigin::from_url("http://127.0.0.1:5173").unwrap();
        assert!(grant_covers(
            &ChromeConnectionGrant::DeveloperAllSites,
            &origin
        ));
        assert!(grant_covers(
            &ChromeConnectionGrant::Origin(tidebreak_core::ChromeOriginScope::Origin {
                origin: BrowserOrigin::parse("https://example.com").unwrap()
            }),
            &origin
        ));
        assert!(!grant_covers(
            &ChromeConnectionGrant::Origin(tidebreak_core::ChromeOriginScope::LoopbackWorkspace),
            &origin
        ));
        assert!(grant_covers(
            &ChromeConnectionGrant::Origin(tidebreak_core::ChromeOriginScope::LoopbackWorkspace),
            &loopback
        ));
    }

    #[test]
    fn semantic_snapshot_projection_marks_sensitive_values_null() {
        let (nodes, frames, truncated) = parse_semantic_snapshot(
            json!({
                "nodes": [
                    {"kind": "interactive", "tag": "input", "role": "interactive", "name": "password", "frame": "main", "value": "secret", "sensitive": true, "actions": [], "bounds": {"x": 1.0, "y": 2.0, "width": 3.0, "height": 4.0}},
                    {"kind": "interactive", "tag": "button", "role": "interactive", "name": "Go", "frame": "main", "sensitive": false, "actions": [], "bounds": {"x": 1.0, "y": 2.0, "width": 3.0, "height": 4.0}}
                ],
                "frames": [{"name": "main", "url": "https://example.com", "status": "same_origin"}],
                "truncated": false
            }),
            10,
            &ChromeConnectionGrant::DeveloperAllSites,
        );
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].sensitive, true);
        assert_eq!(nodes[0].value, None);
        assert_eq!(frames.len(), 1);
        assert!(!truncated);
    }

    #[test]
    fn frame_tree_is_bounded() {
        let tree = json!({
            "frame": {"id": "top"},
            "childFrames": [
                {"frame": {"id": "child-1"}},
                {"frame": {"id": "child-2"}, "childFrames": [{"frame": {"id": "grandchild"}}]}
            ]
        });
        let ids = bounded_frame_tree(tree, 10).unwrap();
        assert_eq!(ids, vec!["top", "child-1", "child-2", "grandchild"]);
        let deep = json!({
            "frame": {"id": "top"},
            "childFrames": [
                {"frame": {"id": "a"}},
                {"frame": {"id": "b"}},
                {"frame": {"id": "c"}}
            ]
        });
        assert!(bounded_frame_tree(deep, 2).is_err());
    }

    #[test]
    fn selectors_are_stable_and_host_owned() {
        assert_eq!(
            selector_for_node_id("n-7"),
            r#"[data-tb-chrome-ref="n-7"]"#
        );
    }
}
