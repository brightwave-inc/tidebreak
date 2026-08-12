//! The computer-use native backend: the seam between the broker (policy) and
//! the macOS-native helper (raw screen capture / accessibility reads / window
//! enumeration / input synthesis).
//!
//! The broker owns every decision — capability check, consent, audit. A backend
//! only performs an already-authorized operation and returns structured data.
//! The shipping backend ([`HelperBackend`]) spawns the signed
//! `openwave-cu-helper` binary, one process per operation, over a small
//! JSON-stdio protocol; the default [`UnsupportedBackend`] (no helper
//! configured / non-macOS) refuses every op so the broker degrades gracefully
//! and the ops stay unadvertised in the `Hello` handshake.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::set_of_marks::Mark;

/// Hard wall-clock bound on a single helper invocation. The broker handles one
/// request at a time (its stdio loop is synchronous), so a hung helper would
/// wedge the entire sidecar. Generous: a screenshot / AX read is normally well
/// under a second, but a busy WindowServer or a slow shareable-content query
/// can take a few seconds; 30s leaves headroom while still guaranteeing
/// recovery.
const HELPER_TIMEOUT: Duration = Duration::from_secs(30);
/// How often to poll the child for exit while waiting.
const HELPER_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Cap on retained helper stdout/stderr. The helper already bounds the AX tree,
/// but never trust it to — a buggy/hostile helper must not OOM the broker.
/// Overflow is drained-and-discarded (so the child never blocks on a full pipe)
/// and the truncated bytes fail JSON parsing into a clean error.
const MAX_HELPER_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

/// Environment override for the helper binary path (development / testing). In
/// a packaged build the broker resolves the helper from a stable bundled
/// location instead.
pub const HELPER_PATH_ENV: &str = "OPENWAVE_CU_HELPER_PATH";

/// What a screen capture targets. Scoped per the broker's capability model: a
/// whole-display capture needs the `Screen` scope; an app capture needs the
/// `App { bundle_id }` scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaptureTarget {
    /// A whole display (the main display when `display_id` is absent).
    Display { display_id: Option<u32> },
    /// Every window of one app, at full display pixel size.
    App { bundle_id: String },
}

/// One on-screen window, as enumerated by the helper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowInfo {
    pub window_id: u32,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub app_name: Option<String>,
    #[serde(default)]
    pub bundle_id: Option<String>,
    pub pid: i32,
    pub frame: WindowFrame,
}

/// On-screen rectangle of a window (global, top-left origin).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowFrame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Why a backend operation failed, so the broker can map it to a retryable flag
/// / consent prompt without string-matching the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendErrorKind {
    /// A required macOS TCC permission (Screen Recording / Accessibility) is
    /// not granted.
    PermissionDenied,
    /// The target app/window/display was not found (e.g. the app is not
    /// running).
    NotFound,
    /// The request was malformed (a broker-side bug — should not happen in
    /// normal flow).
    InvalidRequest,
    /// A control op's target element no longer resolves at its addressed path,
    /// or its fingerprint changed since it was read — the UI shifted. Distinct
    /// from `NotFound` so the agent learns to re-read the accessibility tree
    /// and retry rather than treating it as a hard failure.
    StaleElement,
    /// A safety guard backed off instead of acting (a system
    /// security/authorization dialog owns the foreground). Recorded as denied,
    /// NOT retryable — the agent must surface it and stop, not re-fire input at
    /// a surface the user is mid-authentication on.
    Yielded,
    /// The native operation failed for another reason.
    OperationFailed,
    /// This build/platform has no working backend.
    Unsupported,
}

#[derive(Debug, Clone)]
pub struct BackendError {
    pub kind: BackendErrorKind,
    pub message: String,
}

impl BackendError {
    fn new(kind: BackendErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// The macOS TCC grants computer use needs, as reported by the native helper's
/// preflight (`permission_status`) or after a request (`request_permissions`).
/// Both read `false` on a backend with no working helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionStatus {
    /// Screen Recording — required for `capture_screen`.
    pub screen_recording: bool,
    /// Accessibility — required for `read_ax_tree` and the control
    /// (input-synthesis) ops.
    pub accessibility: bool,
}

/// Metadata for a completed capture. The PNG bytes are written to the
/// broker-provided path; only these dimensions come back over the helper's
/// stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureMeta {
    pub width: u32,
    pub height: u32,
    pub media_type: String,
}

/// A bounded accessibility-tree read. `tree` is opaque nested JSON the helper
/// produced under its node / depth / string caps; the broker passes it through
/// without interpreting it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AxTree {
    pub app_name: Option<String>,
    pub tree: Value,
    pub truncated: bool,
}

/// Where a control op should act: an accessibility element addressed by its
/// `element_id` (the index-path `id` from a prior `read_ax_tree`, plus the
/// `fingerprint` to detect drift), OR a raw coordinate point (`x`/`y`) when the
/// app exposes no usable element. The broker passes this through opaquely; the
/// helper re-resolves the element against the live tree (or synthesizes at the
/// point).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ElementTarget {
    pub element_id: Option<String>,
    pub element_fingerprint: Option<String>,
    pub x: Option<f64>,
    pub y: Option<f64>,
}

/// Outcome of a control op (click/type/key/scroll/focus). `used_fallback` is
/// true when AX targeting was not available and a coordinate/keystroke
/// synthesis was used instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlMeta {
    pub success: bool,
    pub used_fallback: bool,
    pub detail: Option<String>,
}

/// A read-only description of a target element, used by the broker's
/// forced-confirmation tripwire to classify whether a control op is
/// consequential before acting. The fields are normalized so the classifier is
/// platform-independent: on macOS `role` is the `AXRole` and `label` is the
/// `AXTitle`/`AXDescription`; a future Windows backend maps UIA `ControlType` /
/// `Name` onto the same shape. Either field may be absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ElementDescription {
    pub role: Option<String>,
    pub label: Option<String>,
    /// The element's current content fingerprint, so the broker can bind a
    /// confirmation to the exact element it showed. Absent when the element
    /// did not resolve.
    pub fingerprint: Option<String>,
}

/// The native operations the broker delegates after authorizing. Implementers
/// do no policy. `Sync` because the broker shares one backend across its
/// cloned handles.
pub trait ComputerUseBackend: Send + Sync {
    /// Report the macOS TCC permissions computer use needs (Screen Recording,
    /// Accessibility) without prompting — the preflight the desktop
    /// permission-checklist polls. Carries no policy: it reflects the OS grant
    /// state only.
    fn permission_status(&self) -> Result<PermissionStatus, BackendError>;
    /// Actively request those TCC grants, surfacing the native Screen Recording
    /// and Accessibility modals together. Returns the post-request status.
    /// Driven by the user pressing "Enable" in the checklist, never by the
    /// agent.
    fn request_permissions(&self) -> Result<PermissionStatus, BackendError>;
    /// Capture `target` to `out_path` (a broker-owned staging path). Returns
    /// the image dimensions.
    fn capture(&self, target: &CaptureTarget, out_path: &Path)
        -> Result<CaptureMeta, BackendError>;
    /// Capture `target` to `out_path`, optionally drawing numbered Set-of-Marks
    /// badges over the PNG.
    fn capture_with_marks(
        &self,
        target: &CaptureTarget,
        out_path: &Path,
        marks: &[Mark],
    ) -> Result<CaptureMeta, BackendError> {
        let _ = marks;
        self.capture(target, out_path)
    }
    /// Read an app's accessibility tree, bounded by the (clamped) depth / node
    /// budget.
    fn read_ax_tree(
        &self,
        bundle_id: &str,
        max_depth: Option<u32>,
        max_nodes: Option<u32>,
    ) -> Result<AxTree, BackendError>;
    /// Enumerate on-screen windows, optionally filtered to one app.
    fn list_windows(&self, bundle_id: Option<&str>) -> Result<Vec<WindowInfo>, BackendError>;
    /// Click an element (AX press) or coordinate point in an app. `button` is
    /// "left" (default) or "right"; `click_count` 1 (single) or 2 (double).
    fn click(
        &self,
        bundle_id: &str,
        target: &ElementTarget,
        button: Option<&str>,
        click_count: Option<u32>,
    ) -> Result<ControlMeta, BackendError>;
    /// Type text into the targeted element (focus + set value, else synthesize
    /// keystrokes) or, with an empty target, the app's focused field.
    fn type_text(
        &self,
        bundle_id: &str,
        text: &str,
        target: &ElementTarget,
    ) -> Result<ControlMeta, BackendError>;
    /// Press a key (optionally with chord modifiers) in the focused app.
    fn key_press(
        &self,
        bundle_id: &str,
        key: &str,
        modifiers: Option<&[String]>,
    ) -> Result<ControlMeta, BackendError>;
    /// Scroll the targeted element or point by a pixel delta.
    fn scroll(
        &self,
        bundle_id: &str,
        target: &ElementTarget,
        dx: Option<f64>,
        dy: Option<f64>,
    ) -> Result<ControlMeta, BackendError>;
    /// Bring an app (optionally a specific window) to the front.
    fn focus_window(
        &self,
        bundle_id: &str,
        window_id: Option<u32>,
    ) -> Result<ControlMeta, BackendError>;
    /// Read the target element's normalized `{role, label}` without acting —
    /// the trust-independent signal the broker's forced-confirmation tripwire
    /// classifies before a control op runs. Resolves the same element the op
    /// will (by `element_id`), so a stale/missing element surfaces the same way
    /// the op would.
    fn describe_element(
        &self,
        bundle_id: &str,
        target: &ElementTarget,
    ) -> Result<ElementDescription, BackendError>;
    /// Whether this backend can actually perform operations. The broker only
    /// advertises the computer-use ops in `Hello` when this is true.
    fn is_available(&self) -> bool;
}

/// The default backend: no helper available. Every operation is refused;
/// nothing is advertised.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedBackend;

impl UnsupportedBackend {
    fn refuse<T>() -> Result<T, BackendError> {
        Err(BackendError::new(
            BackendErrorKind::Unsupported,
            "computer use is not available on this build",
        ))
    }
}

impl ComputerUseBackend for UnsupportedBackend {
    fn permission_status(&self) -> Result<PermissionStatus, BackendError> {
        Self::refuse()
    }
    fn request_permissions(&self) -> Result<PermissionStatus, BackendError> {
        Self::refuse()
    }
    fn capture(&self, _: &CaptureTarget, _: &Path) -> Result<CaptureMeta, BackendError> {
        Self::refuse()
    }
    fn read_ax_tree(
        &self,
        _: &str,
        _: Option<u32>,
        _: Option<u32>,
    ) -> Result<AxTree, BackendError> {
        Self::refuse()
    }
    fn list_windows(&self, _: Option<&str>) -> Result<Vec<WindowInfo>, BackendError> {
        Self::refuse()
    }
    fn click(
        &self,
        _: &str,
        _: &ElementTarget,
        _: Option<&str>,
        _: Option<u32>,
    ) -> Result<ControlMeta, BackendError> {
        Self::refuse()
    }
    fn type_text(&self, _: &str, _: &str, _: &ElementTarget) -> Result<ControlMeta, BackendError> {
        Self::refuse()
    }
    fn key_press(
        &self,
        _: &str,
        _: &str,
        _: Option<&[String]>,
    ) -> Result<ControlMeta, BackendError> {
        Self::refuse()
    }
    fn scroll(
        &self,
        _: &str,
        _: &ElementTarget,
        _: Option<f64>,
        _: Option<f64>,
    ) -> Result<ControlMeta, BackendError> {
        Self::refuse()
    }
    fn focus_window(&self, _: &str, _: Option<u32>) -> Result<ControlMeta, BackendError> {
        Self::refuse()
    }
    fn describe_element(
        &self,
        _: &str,
        _: &ElementTarget,
    ) -> Result<ElementDescription, BackendError> {
        Self::refuse()
    }
    fn is_available(&self) -> bool {
        false
    }
}

/// The shipping backend: spawns the signed `openwave-cu-helper` binary once per
/// operation, writes one JSON request to its stdin, and reads one JSON response
/// from its stdout.
#[derive(Debug, Clone)]
pub struct HelperBackend {
    helper_path: PathBuf,
    timeout: Duration,
}

impl HelperBackend {
    pub fn new(helper_path: PathBuf) -> Self {
        Self {
            helper_path,
            timeout: HELPER_TIMEOUT,
        }
    }

    /// Resolve the helper binary path: the development / test override first,
    /// then the stable bundled location relative to the broker executable.
    /// Returns `None` when no helper is present (the broker then uses
    /// [`UnsupportedBackend`] and does not advertise the computer-use ops).
    pub fn resolve() -> Option<Self> {
        if let Some(path) = std::env::var_os(HELPER_PATH_ENV) {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Some(Self::new(path));
            }
        }
        // Packaged layout: the helper sits beside the broker under the app
        // bundle's Resources (a stable path so the TCC grants stay bound).
        let exe = std::env::current_exe().ok()?;
        let bundled = exe
            .parent()?
            .parent()?
            .join("Resources")
            .join("host-broker")
            .join("openwave-cu-helper");
        bundled.is_file().then(|| Self::new(bundled))
    }

    /// Construct with an explicit per-invocation timeout (used by tests to
    /// exercise the kill path without waiting the full [`HELPER_TIMEOUT`]).
    #[cfg(all(test, unix))]
    fn with_timeout(helper_path: PathBuf, timeout: Duration) -> Self {
        Self {
            helper_path,
            timeout,
        }
    }

    /// Run one helper invocation: spawn, drain stdout/stderr concurrently,
    /// write `request` to stdin (then EOF), and wait for exit with a timeout —
    /// killing a hung helper so it cannot wedge the single-threaded broker. The
    /// helper replies `{"ok":true,"result":..}` or `{"ok":false,"code":..,..}`.
    fn run(&self, request: Value) -> Result<Value, BackendError> {
        let bytes = serde_json::to_vec(&request).map_err(|e| {
            BackendError::new(
                BackendErrorKind::OperationFailed,
                format!("cannot encode helper request: {e}"),
            )
        })?;

        let mut child = Command::new(&self.helper_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                BackendError::new(
                    BackendErrorKind::OperationFailed,
                    format!("cannot spawn computer-use helper: {e}"),
                )
            })?;

        // Drain stdout/stderr in detached threads before writing stdin, so the
        // child can never block on a full output pipe (the AX tree can exceed
        // the OS pipe buffer) — which would otherwise look like a hang and
        // force a kill. The threads finish when the pipes close (child exit or
        // kill).
        let stdout_reader = spawn_drain(child.stdout.take());
        let stderr_reader = spawn_drain(child.stderr.take());

        // The request is small, so a single blocking write cannot deadlock now
        // that stdout is draining.
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&bytes).map_err(|e| {
                BackendError::new(
                    BackendErrorKind::OperationFailed,
                    format!("cannot write to helper: {e}"),
                )
            })?;
            // stdin drops here → EOF, so the single-shot helper proceeds.
        }

        let timed_out = self.wait_bounded(&mut child)?;

        if timed_out {
            // Do not join the drain threads here: if the helper spawned a child
            // that survived the kill and still holds the output pipe, join()
            // would block until that child exits. The threads are detached and
            // finish when the pipe finally closes; we discard their output on
            // timeout regardless.
            return Err(BackendError::new(
                BackendErrorKind::OperationFailed,
                format!(
                    "computer-use helper timed out after {}s",
                    self.timeout.as_secs()
                ),
            ));
        }

        let stdout = stdout_reader.join().unwrap_or_default();
        let stderr = stderr_reader.join().unwrap_or_default();

        if stdout.is_empty() {
            return Err(BackendError::new(
                BackendErrorKind::OperationFailed,
                format!(
                    "helper produced no output: {}",
                    String::from_utf8_lossy(&stderr)
                ),
            ));
        }

        let envelope: HelperEnvelope = serde_json::from_slice(&stdout).map_err(|e| {
            BackendError::new(
                BackendErrorKind::OperationFailed,
                format!("unparsable helper response: {e}"),
            )
        })?;

        if envelope.ok {
            Ok(envelope.result.unwrap_or(Value::Null))
        } else {
            Err(BackendError::new(
                map_code(envelope.code.as_deref()),
                envelope
                    .error
                    .unwrap_or_else(|| "helper reported failure".to_string()),
            ))
        }
    }

    /// Poll the child for exit up to the timeout; on timeout, kill it (and reap
    /// it) so a hung helper cannot wedge the broker. Returns whether the
    /// timeout fired.
    fn wait_bounded(&self, child: &mut std::process::Child) -> Result<bool, BackendError> {
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_status)) => return Ok(false),
                Ok(None) => {
                    if start.elapsed() >= self.timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Ok(true);
                    }
                    thread::sleep(HELPER_POLL_INTERVAL);
                }
                Err(e) => {
                    return Err(BackendError::new(
                        BackendErrorKind::OperationFailed,
                        format!("waiting on helper failed: {e}"),
                    ))
                }
            }
        }
    }
}

/// Drain a child pipe to EOF on a detached thread, retaining at most
/// [`MAX_HELPER_OUTPUT_BYTES`] (overflow is read-and-discarded so the child
/// never blocks on a full pipe). Returns a handle yielding the retained bytes.
/// `None` (pipe unavailable) yields an empty buffer.
fn spawn_drain<R: Read + Send + 'static>(reader: Option<R>) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut kept = Vec::new();
        if let Some(mut reader) = reader {
            let mut chunk = [0u8; 8192];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if kept.len() < MAX_HELPER_OUTPUT_BYTES {
                            let take = (MAX_HELPER_OUTPUT_BYTES - kept.len()).min(n);
                            kept.extend_from_slice(&chunk[..take]);
                        }
                        // Keep reading past the cap (discarding) so the child
                        // can always finish writing.
                    }
                }
            }
        }
        kept
    })
}

impl ComputerUseBackend for HelperBackend {
    fn permission_status(&self) -> Result<PermissionStatus, BackendError> {
        let result = self.run(json!({ "op": "permissions" }))?;
        parse_permission_status(result)
    }

    fn request_permissions(&self) -> Result<PermissionStatus, BackendError> {
        let result = self.run(json!({ "op": "request_permissions" }))?;
        parse_permission_status(result)
    }

    fn capture(
        &self,
        target: &CaptureTarget,
        out_path: &Path,
    ) -> Result<CaptureMeta, BackendError> {
        self.capture_with_marks(target, out_path, &[])
    }

    fn capture_with_marks(
        &self,
        target: &CaptureTarget,
        out_path: &Path,
        marks: &[Mark],
    ) -> Result<CaptureMeta, BackendError> {
        let mut request = match target {
            CaptureTarget::App { bundle_id } => {
                json!({ "op": "capture", "target": "app", "bundle_id": bundle_id })
            }
            CaptureTarget::Display { display_id } => {
                let mut value = json!({ "op": "capture", "target": "display" });
                if let Some(id) = display_id {
                    value["display_id"] = json!(id);
                }
                value
            }
        };
        request["out_path"] = json!(out_path.to_string_lossy());
        if !marks.is_empty() {
            request["marks"] = json!(marks);
        }

        let result = self.run(request)?;
        let meta: CaptureResultJson = serde_json::from_value(result).map_err(|e| {
            BackendError::new(
                BackendErrorKind::OperationFailed,
                format!("malformed capture result: {e}"),
            )
        })?;
        Ok(CaptureMeta {
            width: meta.width,
            height: meta.height,
            media_type: meta.media_type,
        })
    }

    fn read_ax_tree(
        &self,
        bundle_id: &str,
        max_depth: Option<u32>,
        max_nodes: Option<u32>,
    ) -> Result<AxTree, BackendError> {
        let mut request = json!({ "op": "read_ax_tree", "bundle_id": bundle_id });
        if let Some(depth) = max_depth {
            request["max_depth"] = json!(depth);
        }
        if let Some(nodes) = max_nodes {
            request["max_nodes"] = json!(nodes);
        }

        let result = self.run(request)?;
        let parsed: AxTreeResultJson = serde_json::from_value(result).map_err(|e| {
            BackendError::new(
                BackendErrorKind::OperationFailed,
                format!("malformed ax-tree result: {e}"),
            )
        })?;
        Ok(AxTree {
            app_name: parsed.app_name,
            tree: parsed.tree.unwrap_or(Value::Null),
            truncated: parsed.truncated,
        })
    }

    fn list_windows(&self, bundle_id: Option<&str>) -> Result<Vec<WindowInfo>, BackendError> {
        let mut request = json!({ "op": "list_windows" });
        if let Some(bundle_id) = bundle_id {
            request["bundle_id"] = json!(bundle_id);
        }
        let result = self.run(request)?;
        serde_json::from_value(result).map_err(|e| {
            BackendError::new(
                BackendErrorKind::OperationFailed,
                format!("malformed window list: {e}"),
            )
        })
    }

    fn click(
        &self,
        bundle_id: &str,
        target: &ElementTarget,
        button: Option<&str>,
        click_count: Option<u32>,
    ) -> Result<ControlMeta, BackendError> {
        let mut request = json!({ "op": "click", "bundle_id": bundle_id });
        apply_target(&mut request, target);
        if let Some(button) = button {
            request["button"] = json!(button);
        }
        if let Some(count) = click_count {
            request["click_count"] = json!(count);
        }
        self.run_control(request)
    }

    fn type_text(
        &self,
        bundle_id: &str,
        text: &str,
        target: &ElementTarget,
    ) -> Result<ControlMeta, BackendError> {
        let mut request = json!({ "op": "type_text", "bundle_id": bundle_id, "text": text });
        apply_target(&mut request, target);
        self.run_control(request)
    }

    fn key_press(
        &self,
        bundle_id: &str,
        key: &str,
        modifiers: Option<&[String]>,
    ) -> Result<ControlMeta, BackendError> {
        let mut request = json!({ "op": "key_press", "bundle_id": bundle_id, "key": key });
        if let Some(modifiers) = modifiers {
            request["modifiers"] = json!(modifiers);
        }
        self.run_control(request)
    }

    fn scroll(
        &self,
        bundle_id: &str,
        target: &ElementTarget,
        dx: Option<f64>,
        dy: Option<f64>,
    ) -> Result<ControlMeta, BackendError> {
        let mut request = json!({ "op": "scroll", "bundle_id": bundle_id });
        apply_target(&mut request, target);
        if let Some(dx) = dx {
            request["dx"] = json!(dx);
        }
        if let Some(dy) = dy {
            request["dy"] = json!(dy);
        }
        self.run_control(request)
    }

    fn focus_window(
        &self,
        bundle_id: &str,
        window_id: Option<u32>,
    ) -> Result<ControlMeta, BackendError> {
        let mut request = json!({ "op": "focus_window", "bundle_id": bundle_id });
        if let Some(window_id) = window_id {
            request["window_id"] = json!(window_id);
        }
        self.run_control(request)
    }

    fn describe_element(
        &self,
        bundle_id: &str,
        target: &ElementTarget,
    ) -> Result<ElementDescription, BackendError> {
        let mut request = json!({ "op": "describe_element", "bundle_id": bundle_id });
        apply_target(&mut request, target);
        let result = self.run(request)?;
        let parsed: DescribeResultJson = serde_json::from_value(result).map_err(|e| {
            BackendError::new(
                BackendErrorKind::OperationFailed,
                format!("malformed describe result: {e}"),
            )
        })?;
        Ok(ElementDescription {
            role: parsed.role,
            label: parsed.label,
            fingerprint: parsed.fingerprint,
        })
    }

    fn is_available(&self) -> bool {
        true
    }
}

/// Inject an [`ElementTarget`]'s present fields into a helper request (omitting
/// absent ones so the helper's "exactly one of element_id / point" validation
/// stays meaningful).
fn apply_target(request: &mut Value, target: &ElementTarget) {
    if let Some(id) = &target.element_id {
        request["element_id"] = json!(id);
    }
    if let Some(fingerprint) = &target.element_fingerprint {
        request["element_fingerprint"] = json!(fingerprint);
    }
    if let Some(x) = target.x {
        request["x"] = json!(x);
    }
    if let Some(y) = target.y {
        request["y"] = json!(y);
    }
}

impl HelperBackend {
    /// Run a control op and parse its `{success, used_fallback, detail}`
    /// result.
    fn run_control(&self, request: Value) -> Result<ControlMeta, BackendError> {
        let result = self.run(request)?;
        let parsed: ControlResultJson = serde_json::from_value(result).map_err(|e| {
            BackendError::new(
                BackendErrorKind::OperationFailed,
                format!("malformed control result: {e}"),
            )
        })?;
        Ok(ControlMeta {
            success: parsed.success,
            used_fallback: parsed.used_fallback,
            detail: parsed.detail,
        })
    }
}

/// Parse the helper's `permissions` / `request_permissions` result
/// (`{screen_recording, accessibility}`) into a [`PermissionStatus`].
fn parse_permission_status(result: Value) -> Result<PermissionStatus, BackendError> {
    let parsed: PermissionStatusJson = serde_json::from_value(result).map_err(|e| {
        BackendError::new(
            BackendErrorKind::OperationFailed,
            format!("malformed permission status: {e}"),
        )
    })?;
    Ok(PermissionStatus {
        screen_recording: parsed.screen_recording,
        accessibility: parsed.accessibility,
    })
}

fn map_code(code: Option<&str>) -> BackendErrorKind {
    match code {
        Some("permission_denied") => BackendErrorKind::PermissionDenied,
        Some("not_found") => BackendErrorKind::NotFound,
        Some("invalid_request") => BackendErrorKind::InvalidRequest,
        Some("stale_element") => BackendErrorKind::StaleElement,
        Some("yielded") => BackendErrorKind::Yielded,
        _ => BackendErrorKind::OperationFailed,
    }
}

/// The helper's stdout envelope.
#[derive(Debug, Deserialize)]
struct HelperEnvelope {
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PermissionStatusJson {
    #[serde(default)]
    screen_recording: bool,
    #[serde(default)]
    accessibility: bool,
}

#[derive(Debug, Deserialize)]
struct CaptureResultJson {
    width: u32,
    height: u32,
    media_type: String,
}

#[derive(Debug, Deserialize)]
struct AxTreeResultJson {
    #[serde(default)]
    app_name: Option<String>,
    #[serde(default)]
    tree: Option<Value>,
    #[serde(default)]
    truncated: bool,
}

#[derive(Debug, Deserialize)]
struct ControlResultJson {
    success: bool,
    #[serde(default)]
    used_fallback: bool,
    #[serde(default)]
    detail: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DescribeResultJson {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    fingerprint: Option<String>,
}

// Exercises the HelperBackend's spawn / concurrent-drain / bounded-wait
// machinery against fake shell-script "helpers" (Unix only — the real helper is
// macOS-only, but the IO/timeout logic is platform-agnostic).
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Write an executable `/bin/sh` script to a temp path and return it. The
    /// body is the fake helper.
    fn fake_helper(tag: &str, body: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("openwave-cu-fake-{tag}-{}", std::process::id()));
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn parses_a_well_formed_ok_envelope() {
        let helper = fake_helper("ok", r#"cat >/dev/null; printf '{"ok":true,"result":[]}'"#);
        let windows = HelperBackend::new(helper.clone())
            .list_windows(None)
            .unwrap();
        assert!(windows.is_empty());
        let _ = std::fs::remove_file(&helper);
    }

    #[test]
    fn error_envelope_maps_to_backend_error_kind() {
        let helper = fake_helper(
            "perm",
            r#"cat >/dev/null; printf '{"ok":false,"code":"permission_denied","error":"nope"}'"#,
        );
        let err = HelperBackend::new(helper.clone())
            .list_windows(None)
            .unwrap_err();
        assert_eq!(err.kind, BackendErrorKind::PermissionDenied);
        let _ = std::fs::remove_file(&helper);
    }

    #[test]
    fn yielded_envelope_maps_to_the_yielded_kind() {
        let helper = fake_helper(
            "yield",
            r#"cat >/dev/null; printf '{"ok":false,"code":"yielded","error":"a system security dialog is in the foreground"}'"#,
        );
        let err = HelperBackend::new(helper.clone())
            .list_windows(None)
            .unwrap_err();
        assert_eq!(err.kind, BackendErrorKind::Yielded);
        let _ = std::fs::remove_file(&helper);
    }

    #[test]
    fn a_hung_helper_is_killed_at_the_timeout() {
        // Sleeps far past the timeout; must be killed and reported promptly,
        // not block the broker.
        let helper = fake_helper("hang", "sleep 30");
        let backend = HelperBackend::with_timeout(helper.clone(), Duration::from_millis(150));
        let start = Instant::now();
        let err = backend.list_windows(None).unwrap_err();
        assert_eq!(err.kind, BackendErrorKind::OperationFailed);
        assert!(err.message.contains("timed out"), "got: {}", err.message);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "should return promptly after killing, took {:?}",
            start.elapsed()
        );
        let _ = std::fs::remove_file(&helper);
    }

    #[test]
    fn empty_output_is_a_clean_error() {
        let helper = fake_helper("empty", "cat >/dev/null; exit 0");
        let err = HelperBackend::new(helper.clone())
            .list_windows(None)
            .unwrap_err();
        assert_eq!(err.kind, BackendErrorKind::OperationFailed);
        assert!(err.message.contains("no output"), "got: {}", err.message);
        let _ = std::fs::remove_file(&helper);
    }

    #[test]
    fn large_output_is_drained_without_deadlocking() {
        // ~200 KB of output exceeds the OS pipe buffer; the concurrent drain
        // must keep the child from blocking on a full pipe.
        let helper = fake_helper(
            "big",
            r#"cat >/dev/null; printf '{"ok":true,"result":"'; head -c 200000 /dev/zero | tr '\0' a; printf '"}'"#,
        );
        let result = HelperBackend::new(helper.clone())
            .run(serde_json::json!({ "op": "noop" }))
            .unwrap();
        assert!(result.is_string());
        assert_eq!(result.as_str().unwrap().len(), 200_000);
        let _ = std::fs::remove_file(&helper);
    }
}
