//! The computer-use native backend: the seam between the broker (policy) and
//! the macOS-native helper (raw screen capture / accessibility reads / window
//! enumeration / input synthesis).
//!
//! The broker owns every decision — capability check, consent, audit. A backend
//! only performs an already-authorized operation and returns structured data.
//! The shipping backend ([`HelperBackend`]) spawns the signed
//! `tidebreak-cu-helper` binary, one process per operation, over a small
//! JSON-stdio protocol; the default [`UnsupportedBackend`] (no helper
//! configured / non-macOS) refuses every op so the broker degrades gracefully
//! and the ops stay unadvertised in the `Hello` handshake.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::set_of_marks::Mark;

mod input_recovery;
use input_recovery::InputRecovery;

/// Hard wall-clock bound on a single helper invocation. The broker handles one
/// request at a time (its stdio loop is synchronous), so a hung helper would
/// wedge the entire sidecar. Generous: a screenshot / AX read is normally well
/// under a second, but a busy WindowServer or a slow shareable-content query
/// can take a few seconds; 40s leaves headroom for a 30s condition wait while guaranteeing
/// recovery.
const HELPER_TIMEOUT: Duration = Duration::from_secs(40);
/// How often to poll the child for exit while waiting.
const HELPER_POLL_INTERVAL: Duration = Duration::from_millis(50);
const HELPER_CANCEL_GRACE: Duration = Duration::from_secs(2);
const HELPER_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
/// Keep the desktop response deadline beyond operation cancellation and recovery.
/// Process startup and protocol overhead need an additional margin at the caller.
pub const HELPER_MANAGED_TIMEOUT: Duration = Duration::from_secs(
    HELPER_TIMEOUT.as_secs() + HELPER_CANCEL_GRACE.as_secs() + HELPER_CLEANUP_TIMEOUT.as_secs(),
);
/// Cap on retained helper stdout/stderr. The helper already bounds the AX tree,
/// but never trust it to — a buggy/hostile helper must not OOM the broker.
/// Overflow is drained-and-discarded (so the child never blocks on a full pipe)
/// and the truncated bytes fail JSON parsing into a clean error.
const MAX_HELPER_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

/// Environment override for the helper binary path (development / testing). In
/// a packaged build the broker resolves the helper from a stable bundled
/// location instead.
pub const HELPER_PATH_ENV: &str = "TIDEBREAK_CU_HELPER_PATH";
/// Host-owned cancellation generation, never supplied by a model tool call.
pub const HELPER_CANCEL_PATH_ENV: &str = "TIDEBREAK_CU_CANCEL_PATH";

/// How a control op interacts with the user's live session. `Background` (the
/// canonical default everywhere on this path) instructs the helper to act on
/// the app directly while preserving the user's focus, pointer, and active
/// window; a helper that cannot honor that refuses with `requires_foreground`
/// instead of taking over. `Foreground` is an explicit takeover the desktop
/// only dispatches after its own separate trusted approval — the broker and
/// helper never escalate a background request on their own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    #[default]
    Background,
    Foreground,
}

impl ExecutionMode {
    /// The wire value the helper protocol uses.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Foreground => "foreground",
        }
    }
}

/// What a screen capture targets. Scoped per the broker's capability model: a
/// whole-display capture needs the `Screen` scope; an app capture needs the
/// `App { bundle_id }` scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaptureTarget {
    /// A whole display (the main display when `display_id` is absent).
    Display { display_id: Option<u32> },
    /// One app. `window_id` selects one on-screen window of that app; when
    /// absent the capture includes every on-screen window of the app. The
    /// image may be downscaled to a bounded long edge.
    App {
        bundle_id: String,
        window_id: Option<u32>,
    },
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
    /// A raw coordinate target was refused because it does not fall inside an
    /// on-screen window owned by the granted app. The broker did not act.
    TargetOutsideApp,
    /// A background-mode control op could not be performed without taking over
    /// the user's focus or pointer, so the helper refused instead of acting.
    /// Nothing ran; the agent must not retry automatically. A foreground
    /// re-issue is a deliberate escalation that needs the user's separate
    /// takeover approval.
    RequiresForeground,
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
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureMeta {
    pub width: u32,
    pub height: u32,
    pub media_type: String,
    /// Screenshot crop in global top-left logical coordinates.
    pub coordinate_frame: Option<WindowFrame>,
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

/// A deterministic condition the native helper evaluates against live app /
/// window / accessibility state. The broker never interprets screen content
/// itself; the helper's read-only tree scan is the sole source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitCondition {
    /// A process with this bundle id is a running application.
    AppRunning,
    /// The app owns at least one on-screen window.
    WindowVisible,
    /// The app's bounded accessibility tree contains this exact text in an
    /// element title/description/value.
    TextPresent { text: String },
    /// The app's bounded accessibility tree no longer contains this exact
    /// text.
    TextAbsent { text: String },
}

/// Outcome of one native condition check after the requested bounded poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitObservation {
    pub met: bool,
    pub timed_out: bool,
}

/// Outcome of a control op (click/type/key/scroll/focus). `used_fallback` is
/// true when AX targeting was not available and a coordinate/keystroke
/// synthesis was used instead. `execution_mode` is the mode the helper
/// actually performed the action in, reported truthfully rather than echoing
/// the request — absent when the helper predates the mode contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlMeta {
    pub success: bool,
    pub used_fallback: bool,
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_mode: Option<ExecutionMode>,
    /// Verified action geometry from the native helper, used only for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Value>,
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
        max_dimension: Option<u32>,
    ) -> Result<CaptureMeta, BackendError> {
        let _ = marks;
        let _ = max_dimension;
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
    /// `mode` (like every control op below) tells the helper whether it must
    /// preserve the user's focus/pointer (background) or may take over
    /// (foreground, already user-approved by the trusted desktop).
    fn click(
        &self,
        bundle_id: &str,
        target: &ElementTarget,
        button: Option<&str>,
        click_count: Option<u32>,
        mode: ExecutionMode,
    ) -> Result<ControlMeta, BackendError>;
    /// Type text into the targeted element (focus + set value, else synthesize
    /// keystrokes) or, with an empty target, the app's focused field.
    fn type_text(
        &self,
        bundle_id: &str,
        text: &str,
        target: &ElementTarget,
        mode: ExecutionMode,
    ) -> Result<ControlMeta, BackendError>;
    /// Press a key (optionally with chord modifiers) in the focused app.
    fn key_press(
        &self,
        bundle_id: &str,
        key: &str,
        modifiers: Option<&[String]>,
        mode: ExecutionMode,
    ) -> Result<ControlMeta, BackendError>;
    /// Scroll the targeted element or point by a pixel delta.
    fn scroll(
        &self,
        bundle_id: &str,
        target: &ElementTarget,
        dx: Option<f64>,
        dy: Option<f64>,
        mode: ExecutionMode,
    ) -> Result<ControlMeta, BackendError>;
    /// Bring an app (optionally a specific window) to the front.
    fn focus_window(
        &self,
        bundle_id: &str,
        window_id: Option<u32>,
        mode: ExecutionMode,
    ) -> Result<ControlMeta, BackendError>;
    /// Launch the registered application for `bundle_id` via `NSWorkspace`.
    /// No path, executable, or argument ever reaches this method.
    fn launch_app(&self, bundle_id: &str, mode: ExecutionMode)
        -> Result<ControlMeta, BackendError>;
    /// Move the pointer over an element or confined coordinate point without
    /// pressing any button.
    fn hover(
        &self,
        bundle_id: &str,
        target: &ElementTarget,
        mode: ExecutionMode,
    ) -> Result<ControlMeta, BackendError>;
    /// Press at `from`, drag through bounded steps, and release at `to`.
    /// Both endpoints are resolved/confined before the first mouse-down.
    fn drag(
        &self,
        bundle_id: &str,
        from: &ElementTarget,
        to: &ElementTarget,
        duration_ms: Option<u64>,
        mode: ExecutionMode,
    ) -> Result<ControlMeta, BackendError>;
    /// Resize one window of the app to `width` × `height` logical points.
    fn resize_window(
        &self,
        bundle_id: &str,
        window_id: Option<u32>,
        width: f64,
        height: f64,
        mode: ExecutionMode,
    ) -> Result<ControlMeta, BackendError>;
    /// Poll `condition` for up to `timeout_seconds` (mirrors
    /// [`WaitObservation`]). Pure observation: never synthesizes input.
    fn wait_condition(
        &self,
        bundle_id: &str,
        condition: &WaitCondition,
        timeout_seconds: f64,
    ) -> Result<WaitObservation, BackendError>;
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
        _: ExecutionMode,
    ) -> Result<ControlMeta, BackendError> {
        Self::refuse()
    }
    fn type_text(
        &self,
        _: &str,
        _: &str,
        _: &ElementTarget,
        _: ExecutionMode,
    ) -> Result<ControlMeta, BackendError> {
        Self::refuse()
    }
    fn key_press(
        &self,
        _: &str,
        _: &str,
        _: Option<&[String]>,
        _: ExecutionMode,
    ) -> Result<ControlMeta, BackendError> {
        Self::refuse()
    }
    fn scroll(
        &self,
        _: &str,
        _: &ElementTarget,
        _: Option<f64>,
        _: Option<f64>,
        _: ExecutionMode,
    ) -> Result<ControlMeta, BackendError> {
        Self::refuse()
    }
    fn focus_window(
        &self,
        _: &str,
        _: Option<u32>,
        _: ExecutionMode,
    ) -> Result<ControlMeta, BackendError> {
        Self::refuse()
    }
    fn launch_app(&self, _: &str, _: ExecutionMode) -> Result<ControlMeta, BackendError> {
        Self::refuse()
    }
    fn hover(
        &self,
        _: &str,
        _: &ElementTarget,
        _: ExecutionMode,
    ) -> Result<ControlMeta, BackendError> {
        Self::refuse()
    }
    fn drag(
        &self,
        _: &str,
        _: &ElementTarget,
        _: &ElementTarget,
        _: Option<u64>,
        _: ExecutionMode,
    ) -> Result<ControlMeta, BackendError> {
        Self::refuse()
    }
    fn resize_window(
        &self,
        _: &str,
        _: Option<u32>,
        _: f64,
        _: f64,
        _: ExecutionMode,
    ) -> Result<ControlMeta, BackendError> {
        Self::refuse()
    }
    fn wait_condition(
        &self,
        _: &str,
        _: &WaitCondition,
        _: f64,
    ) -> Result<WaitObservation, BackendError> {
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

/// The shipping backend: spawns the signed `tidebreak-cu-helper` binary once per
/// operation, writes one JSON request to its stdin, and reads one JSON response
/// from its stdout.
#[derive(Debug, Clone)]
pub struct HelperBackend {
    helper_path: PathBuf,
    timeout: Duration,
    input_cleanup_failed: Arc<AtomicBool>,
}

impl HelperBackend {
    pub fn new(helper_path: PathBuf) -> Self {
        Self {
            helper_path,
            timeout: HELPER_TIMEOUT,
            input_cleanup_failed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Resolve the helper binary path: the development / test override first,
    /// then the stable bundled location relative to the broker executable.
    /// Returns `None` when no helper is present (the broker then uses
    /// [`UnsupportedBackend`] and does not advertise the computer-use ops).
    pub fn resolve() -> Option<Self> {
        if !computer_use_platform_supported() {
            return None;
        }
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
            .join("tidebreak-cu-helper");
        bundled.is_file().then(|| Self::new(bundled))
    }

    /// Construct with an explicit per-invocation timeout (used by tests to
    /// exercise the kill path without waiting the full [`HELPER_TIMEOUT`]).
    #[cfg(all(test, unix))]
    fn with_timeout(helper_path: PathBuf, timeout: Duration) -> Self {
        Self {
            helper_path,
            timeout,
            input_cleanup_failed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Run one helper invocation: spawn, drain stdout/stderr concurrently,
    /// write `request` to stdin (then EOF), and wait for exit with a timeout —
    /// killing a hung helper so it cannot wedge the single-threaded broker. The
    /// helper replies `{"ok":true,"result":..}` or `{"ok":false,"code":..,..}`.
    fn run(&self, mut request: Value) -> Result<Value, BackendError> {
        if let Some(path) = std::env::var_os(HELPER_CANCEL_PATH_ENV) {
            attach_input_cancellation(&mut request, Path::new(&path))?;
        }
        if matches!(
            request.get("op").and_then(Value::as_str),
            Some("click" | "type_text" | "key_press" | "scroll" | "hover" | "drag")
        ) && self.input_cleanup_failed.load(Ordering::Acquire)
        {
            return Err(BackendError::new(
                BackendErrorKind::OperationFailed,
                "Computer control remains stopped because input cleanup failed.",
            ));
        }
        let recovery = InputRecovery::prepare(&mut request)?;
        let result = self.invoke(&request, self.timeout, recovery.as_ref());
        if let Some(recovery) = recovery {
            let cleanup = recovery.pending().and_then(|pending| {
                if pending {
                    self.invoke(&recovery.cleanup_request(), HELPER_CLEANUP_TIMEOUT, None)?;
                    if recovery.pending()? {
                        return Err(BackendError::new(
                            BackendErrorKind::OperationFailed,
                            "The helper did not finish releasing its recorded input.",
                        ));
                    }
                }
                Ok(pending)
            });
            if let Err(error) = cleanup {
                self.input_cleanup_failed.store(true, Ordering::Release);
                let journal_directory = recovery.preserve();
                eprintln!(
                    "computer-use recovery retained at {}",
                    journal_directory.display()
                );
                return Err(BackendError::new(
                    BackendErrorKind::OperationFailed,
                    format!("Computer control stopped after input cleanup failed: {}. Release any held mouse buttons or keys before restarting Tidebreak.", error.message),
                ));
            }
            if matches!(cleanup, Ok(true)) {
                return Err(BackendError::new(
                    BackendErrorKind::OperationFailed,
                    "The helper required input recovery; the operation outcome is uncertain.",
                ));
            }
        }
        result
    }

    fn invoke(
        &self,
        request: &Value,
        timeout: Duration,
        recovery: Option<&InputRecovery>,
    ) -> Result<Value, BackendError> {
        let bytes = serde_json::to_vec(request).map_err(|e| {
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
        if let Some(recovery) = recovery {
            recovery.worker_started();
        }

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
            if let Err(error) = stdin.write_all(&bytes) {
                let _ = child.kill();
                if child.wait().is_ok() {
                    if let Some(recovery) = recovery {
                        recovery.worker_exited();
                    }
                }
                return Err(BackendError::new(
                    BackendErrorKind::OperationFailed,
                    format!("cannot write to helper: {error}"),
                ));
            }
            // stdin drops here → EOF, so the single-shot helper proceeds.
        }

        let timed_out = self.wait_bounded(&mut child, timeout, recovery)?;

        if timed_out {
            // Do not join the drain threads here: if the helper spawned a child
            // that survived the kill and still holds the output pipe, join()
            // would block until that child exits. The threads are detached and
            // finish when the pipe finally closes; we discard their output on
            // timeout regardless.
            return Err(BackendError::new(
                BackendErrorKind::OperationFailed,
                format!("computer-use helper timed out after {}s", timeout.as_secs()),
            ));
        }

        if !child.wait().is_ok_and(|status| status.success()) {
            return Err(BackendError::new(
                BackendErrorKind::OperationFailed,
                "The computer-use helper exited before completing its operation.",
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
    fn wait_bounded(
        &self,
        child: &mut std::process::Child,
        timeout: Duration,
        recovery: Option<&InputRecovery>,
    ) -> Result<bool, BackendError> {
        let start = Instant::now();
        let mut cancelled_at = None;
        loop {
            match child.try_wait() {
                Ok(Some(_status)) => {
                    if let Some(recovery) = recovery {
                        recovery.worker_exited();
                    }
                    return Ok(cancelled_at.is_some());
                }
                Ok(None) => {
                    if start.elapsed() >= timeout {
                        if cancelled_at.is_none()
                            && recovery.is_some_and(|state| state.cancel().is_ok())
                        {
                            cancelled_at = Some(Instant::now());
                        }
                        if cancelled_at.is_none_or(|at| at.elapsed() >= HELPER_CANCEL_GRACE) {
                            child.kill().map_err(|error| {
                                BackendError::new(
                                    BackendErrorKind::OperationFailed,
                                    format!("cannot stop helper: {error}"),
                                )
                            })?;
                            child.wait().map_err(|error| {
                                BackendError::new(
                                    BackendErrorKind::OperationFailed,
                                    format!("cannot reap helper: {error}"),
                                )
                            })?;
                            if let Some(recovery) = recovery {
                                recovery.worker_exited();
                            }
                            return Ok(true);
                        }
                    }
                    thread::sleep(HELPER_POLL_INTERVAL);
                }
                Err(e) => {
                    let _ = child.kill();
                    if child.wait().is_ok() {
                        if let Some(recovery) = recovery {
                            recovery.worker_exited();
                        }
                    }
                    return Err(BackendError::new(
                        BackendErrorKind::OperationFailed,
                        format!("waiting on helper failed: {e}"),
                    ));
                }
            }
        }
    }
}

/// Read the host's generation before spawning input. A stopped or missing
/// generation refuses input even when Stop races the broker's dispatch queue.
fn attach_input_cancellation(request: &mut Value, path: &Path) -> Result<(), BackendError> {
    if !matches!(
        request.get("op").and_then(Value::as_str),
        Some(
            "click"
                | "type_text"
                | "key_press"
                | "scroll"
                | "focus_window"
                | "launch_app"
                | "hover"
                | "drag"
                | "resize_window"
                | "wait_condition"
        )
    ) {
        return Ok(());
    }
    let generation = std::fs::read_to_string(path).map_err(|_| {
        BackendError::new(
            BackendErrorKind::Yielded,
            "Computer control is stopped or unavailable.",
        )
    })?;
    if uuid::Uuid::parse_str(generation.trim()).is_err() {
        return Err(BackendError::new(
            BackendErrorKind::Yielded,
            "Computer control was stopped.",
        ));
    }
    request["cancel_path"] = json!(path.to_string_lossy());
    request["cancel_generation"] = json!(generation.trim());
    Ok(())
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
        self.capture_with_marks(target, out_path, &[], None)
    }

    fn capture_with_marks(
        &self,
        target: &CaptureTarget,
        out_path: &Path,
        marks: &[Mark],
        max_dimension: Option<u32>,
    ) -> Result<CaptureMeta, BackendError> {
        let mut request = match target {
            CaptureTarget::App {
                bundle_id,
                window_id,
            } => {
                let mut value = json!({ "op": "capture", "target": "app", "bundle_id": bundle_id });
                if let Some(window_id) = window_id {
                    value["window_id"] = json!(window_id);
                }
                value
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
        if let Some(max_dimension) = max_dimension {
            request["max_dimension"] = json!(max_dimension);
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
            coordinate_frame: meta.coordinate_frame,
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
        mode: ExecutionMode,
    ) -> Result<ControlMeta, BackendError> {
        let mut request = json!({
            "op": "click",
            "bundle_id": bundle_id,
            "execution_mode": mode.as_str()
        });
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
        mode: ExecutionMode,
    ) -> Result<ControlMeta, BackendError> {
        let mut request = json!({
            "op": "type_text",
            "bundle_id": bundle_id,
            "text": text,
            "execution_mode": mode.as_str()
        });
        apply_target(&mut request, target);
        self.run_control(request)
    }

    fn key_press(
        &self,
        bundle_id: &str,
        key: &str,
        modifiers: Option<&[String]>,
        mode: ExecutionMode,
    ) -> Result<ControlMeta, BackendError> {
        let mut request = json!({
            "op": "key_press",
            "bundle_id": bundle_id,
            "key": key,
            "execution_mode": mode.as_str()
        });
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
        mode: ExecutionMode,
    ) -> Result<ControlMeta, BackendError> {
        let mut request = json!({
            "op": "scroll",
            "bundle_id": bundle_id,
            "execution_mode": mode.as_str()
        });
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
        mode: ExecutionMode,
    ) -> Result<ControlMeta, BackendError> {
        let mut request = json!({
            "op": "focus_window",
            "bundle_id": bundle_id,
            "execution_mode": mode.as_str()
        });
        if let Some(window_id) = window_id {
            request["window_id"] = json!(window_id);
        }
        self.run_control(request)
    }

    fn launch_app(
        &self,
        bundle_id: &str,
        mode: ExecutionMode,
    ) -> Result<ControlMeta, BackendError> {
        self.run_control(json!({
            "op": "launch_app",
            "bundle_id": bundle_id,
            "execution_mode": mode.as_str()
        }))
    }

    fn hover(
        &self,
        bundle_id: &str,
        target: &ElementTarget,
        mode: ExecutionMode,
    ) -> Result<ControlMeta, BackendError> {
        let mut request = json!({
            "op": "hover",
            "bundle_id": bundle_id,
            "execution_mode": mode.as_str()
        });
        apply_target(&mut request, target);
        self.run_control(request)
    }

    fn drag(
        &self,
        bundle_id: &str,
        from: &ElementTarget,
        to: &ElementTarget,
        duration_ms: Option<u64>,
        mode: ExecutionMode,
    ) -> Result<ControlMeta, BackendError> {
        let mut request = json!({
            "op": "drag",
            "bundle_id": bundle_id,
            "execution_mode": mode.as_str()
        });
        // Helper translates `from_*` / `to_*` snake_case fields back onto the
        // same explicit-point semantics as every other control op.
        apply_target_to(&mut request, "from", from);
        apply_target_to(&mut request, "to", to);
        if let Some(duration_ms) = duration_ms {
            request["duration_ms"] = json!(duration_ms);
        }
        self.run_control(request)
    }

    fn resize_window(
        &self,
        bundle_id: &str,
        window_id: Option<u32>,
        width: f64,
        height: f64,
        mode: ExecutionMode,
    ) -> Result<ControlMeta, BackendError> {
        let mut request = json!({
            "op": "resize_window",
            "bundle_id": bundle_id,
            "width": width,
            "height": height,
            "execution_mode": mode.as_str()
        });
        if let Some(window_id) = window_id {
            request["window_id"] = json!(window_id);
        }
        self.run_control(request)
    }

    fn wait_condition(
        &self,
        bundle_id: &str,
        condition: &WaitCondition,
        timeout_seconds: f64,
    ) -> Result<WaitObservation, BackendError> {
        let (kind, text) = match condition {
            WaitCondition::AppRunning => ("app_running", None),
            WaitCondition::WindowVisible => ("window_visible", None),
            WaitCondition::TextPresent { text } => ("text_present", Some(text.as_str())),
            WaitCondition::TextAbsent { text } => ("text_absent", Some(text.as_str())),
        };
        let mut request = json!({
            "op": "wait_condition",
            "bundle_id": bundle_id,
            "condition": kind,
            "timeout_seconds": timeout_seconds
        });
        if let Some(text) = text {
            request["text"] = json!(text);
        }
        let result = self.run(request)?;
        let parsed: WaitResultJson = serde_json::from_value(result).map_err(|e| {
            BackendError::new(
                BackendErrorKind::OperationFailed,
                format!("malformed wait result: {e}"),
            )
        })?;
        Ok(WaitObservation {
            met: parsed.met,
            timed_out: parsed.timed_out,
        })
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
        computer_use_platform_supported()
    }
}

/// The Swift helper requires macOS 14 even though the desktop supports older releases.
fn computer_use_platform_supported() -> bool {
    #[cfg(target_os = "macos")]
    {
        static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *SUPPORTED.get_or_init(|| {
            Command::new("/usr/bin/sw_vers")
                .arg("-productVersion")
                .env("SYSTEM_VERSION_COMPAT", "0")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .is_some_and(|version| macos_version_supports_computer_use(&version))
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

#[cfg(any(target_os = "macos", test))]
fn macos_version_supports_computer_use(version: &str) -> bool {
    let mut components = version.trim().split('.');
    components
        .next()
        .and_then(|major| major.parse::<u32>().ok())
        .is_some_and(|major| major >= 14)
        && components.all(|component| component.parse::<u32>().is_ok())
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

/// Inject an [`ElementTarget`]'s present fields with a `prefix_` naming so a
/// drag carries two independent addresses (`from_*`/`to_*`) whose semantics
/// are unchanged from the single-target form.
fn apply_target_to(request: &mut Value, prefix: &str, target: &ElementTarget) {
    if let Some(id) = &target.element_id {
        request[format!("{prefix}_element_id")] = json!(id);
    }
    if let Some(fingerprint) = &target.element_fingerprint {
        request[format!("{prefix}_element_fingerprint")] = json!(fingerprint);
    }
    if let Some(x) = target.x {
        request[format!("{prefix}_x")] = json!(x);
    }
    if let Some(y) = target.y {
        request[format!("{prefix}_y")] = json!(y);
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
            execution_mode: parsed.execution_mode,
            cursor: parsed.cursor,
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
        Some("unsupported") => BackendErrorKind::Unsupported,
        Some("permission_denied") => BackendErrorKind::PermissionDenied,
        Some("not_found") => BackendErrorKind::NotFound,
        Some("invalid_request") => BackendErrorKind::InvalidRequest,
        Some("stale_element") => BackendErrorKind::StaleElement,
        Some("requires_foreground" | "independent_input_unavailable") => {
            BackendErrorKind::RequiresForeground
        }
        Some("yielded") => BackendErrorKind::Yielded,
        Some("target_outside_app") => BackendErrorKind::TargetOutsideApp,
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
    #[serde(default)]
    coordinate_frame: Option<WindowFrame>,
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
    /// The mode the helper actually performed in — truthful result metadata,
    /// not an echo of the request.
    #[serde(default)]
    execution_mode: Option<ExecutionMode>,
    #[serde(default)]
    cursor: Option<Value>,
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

#[cfg(test)]
mod platform_tests {
    use super::macos_version_supports_computer_use;

    #[test]
    fn computer_use_requires_macos_14_or_later() {
        for version in ["10.15.7", "10.16", "11.7.10", "12.7.6", "13.7.8"] {
            assert!(!macos_version_supports_computer_use(version), "{version}");
        }
        for version in ["14", "14.0", "14.8.1\n", "15.6.1", "26.0"] {
            assert!(macos_version_supports_computer_use(version), "{version}");
        }
    }

    #[test]
    fn unreadable_macos_versions_do_not_advertise_computer_use() {
        for version in [
            "",
            "macOS 14.0",
            "14.x",
            "14.",
            "14..0",
            "99999999999999999999",
        ] {
            assert!(!macos_version_supports_computer_use(version), "{version}");
        }
    }
}

#[derive(Debug, Deserialize)]
struct WaitResultJson {
    met: bool,
    timed_out: bool,
}

// Exercises the HelperBackend's spawn / concurrent-drain / bounded-wait
// machinery against fake shell-script "helpers" (Unix only — the real helper is
// macOS-only, but the IO/timeout logic is platform-agnostic).
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct FakeHelper {
        _dir: tempfile::TempDir,
        path: PathBuf,
    }

    /// Write an executable `/bin/sh` script in a uniquely owned temporary
    /// directory. Keeping the directory alive prevents fixture paths from
    /// colliding with concurrent or retried test processes.
    fn fake_helper(tag: &str, body: &str) -> FakeHelper {
        let dir = tempfile::Builder::new()
            .prefix(&format!("tidebreak-cu-fake-{tag}-"))
            .tempdir()
            .unwrap();
        let path = dir.path().join("helper");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        FakeHelper { _dir: dir, path }
    }

    fn recovery_helper(behavior: &str) -> FakeHelper {
        let script = r#"
request=$(cat)
field() { printf '%s' "$request" | sed -n "s/.*\"$1\":\"\([^\"]*\)\".*/\1/p"; }
root=$(dirname "$0")
op=$(field op)
journal=$(field input_journal_path)
invocation=$(field input_invocation_id)
cancel=$(field input_cancel_path)
empty() { printf '{"invocation_id":"%s","held":[]}' "$invocation" > "$journal"; }
if [ "$op" = release_recorded_input ]; then
    original=$(cat "$root/pid")
    if kill -0 "$original" 2>/dev/null; then exit 9; fi
    echo cleanup >> "$root/events"
    if [ BEHAVIOR = cleanup_failure ]; then
        printf '{"ok":false,"code":"operation_failed","error":"fixture cleanup failed"}'
    else
        empty
        printf '{"ok":true,"result":{"released":1}}'
    fi
    exit 0
fi
if [ "$op" = list_windows ]; then
    echo observation >> "$root/events"
    printf '{"ok":true,"result":[]}'
    exit 0
fi
echo operation >> "$root/events"
echo $$ > "$root/pid"
printf '%s' "$journal" > "$root/journal"
printf '{"invocation_id":"%s","held":[{"kind":"mouse","code":0}]}' "$invocation" > "$journal"
if [ BEHAVIOR = graceful ]; then
    while [ "$(cat "$cancel")" != stopped ]; do sleep 0.02; done
    echo cancelled >> "$root/events"
    empty
    printf '{"ok":false,"code":"yielded","error":"cancelled"}'
    exit 0
fi
if [ BEHAVIOR = timeout ]; then
    while :; do sleep 0.02; done
fi
if [ BEHAVIOR = claimed_success ]; then
    printf '{"ok":true,"result":{"success":true}}'
    exit 0
fi
if [ BEHAVIOR = structured_error ]; then
    empty
    printf '{"ok":false,"code":"permission_denied","error":"fixture refused"}'
    exit 0
fi
exit 7
"#;
        fake_helper("recovery", &script.replace("BEHAVIOR", behavior))
    }

    fn input_fixture(backend: &HelperBackend) -> Result<Value, BackendError> {
        backend.run(json!({"op": "drag", "execution_mode": "background"}))
    }

    fn recovery_events(helper: &FakeHelper) -> String {
        std::fs::read_to_string(helper._dir.path().join("events")).unwrap()
    }

    #[test]
    fn timeout_cancels_the_invocation_before_forcing_release() {
        let helper = recovery_helper("graceful");
        // Let the shell fixture start under parallel test load before testing cancellation.
        let backend = HelperBackend::with_timeout(helper.path.clone(), Duration::from_millis(500));
        let error = input_fixture(&backend).unwrap_err();
        assert!(error.message.contains("timed out"));
        assert_eq!(recovery_events(&helper), "operation\ncancelled\n");
        assert!(!backend.input_cleanup_failed.load(Ordering::Acquire));
    }

    #[test]
    fn killed_and_crashed_helpers_finish_cleanup_before_the_next_invocation() {
        for behavior in ["timeout", "crash"] {
            let helper = recovery_helper(behavior);
            let backend =
                HelperBackend::with_timeout(helper.path.clone(), Duration::from_millis(75));
            assert!(input_fixture(&backend).is_err());
            assert_eq!(recovery_events(&helper), "operation\ncleanup\n");
            assert!(input_fixture(&backend).is_err());
            assert_eq!(
                recovery_events(&helper),
                "operation\ncleanup\noperation\ncleanup\n"
            );
            assert!(!backend.input_cleanup_failed.load(Ordering::Acquire));
        }
    }

    #[test]
    fn failed_cleanup_preserves_the_journal_and_stops_input_but_allows_observation() {
        let helper = recovery_helper("cleanup_failure");
        let backend = HelperBackend::new(helper.path.clone());
        let error = input_fixture(&backend).unwrap_err();
        assert!(error
            .message
            .contains("Release any held mouse buttons or keys"));
        let path =
            PathBuf::from(std::fs::read_to_string(helper._dir.path().join("journal")).unwrap());
        assert!(path.is_file());
        assert!(input_fixture(&backend)
            .unwrap_err()
            .message
            .contains("remains stopped"));
        assert!(backend.list_windows(None).unwrap().is_empty());
        assert_eq!(
            recovery_events(&helper),
            "operation\ncleanup\nobservation\n"
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn claimed_success_with_held_input_becomes_uncertain_after_recovery() {
        let helper = recovery_helper("claimed_success");
        let backend = HelperBackend::new(helper.path.clone());
        let error = input_fixture(&backend).unwrap_err();
        assert!(error.message.contains("outcome is uncertain"));
        assert_eq!(recovery_events(&helper), "operation\ncleanup\n");
    }

    #[test]
    fn a_structured_error_keeps_its_code_after_input_is_released() {
        let helper = recovery_helper("structured_error");
        let backend = HelperBackend::new(helper.path.clone());
        assert_eq!(
            input_fixture(&backend).unwrap_err().kind,
            BackendErrorKind::PermissionDenied
        );
        assert_eq!(recovery_events(&helper), "operation\n");
    }

    #[test]
    fn stopped_generation_refuses_input_but_preserves_observation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("generation");
        let mut drag = json!({"op": "drag"});
        assert_eq!(
            attach_input_cancellation(&mut drag, &path)
                .unwrap_err()
                .kind,
            BackendErrorKind::Yielded
        );
        std::fs::write(&path, "stopped").unwrap();
        assert!(attach_input_cancellation(&mut drag, &path).is_err());
        let mut read = json!({"op": "read_ax_tree"});
        attach_input_cancellation(&mut read, &path).unwrap();
        assert!(read.get("cancel_generation").is_none());
        let generation = uuid::Uuid::new_v4().to_string();
        std::fs::write(&path, &generation).unwrap();
        attach_input_cancellation(&mut drag, &path).unwrap();
        assert_eq!(drag["cancel_generation"], generation);
        assert_eq!(drag["cancel_path"], path.to_string_lossy().as_ref());
    }

    #[test]
    fn parses_a_well_formed_ok_envelope() {
        let helper = fake_helper("ok", r#"cat >/dev/null; printf '{"ok":true,"result":[]}'"#);
        let windows = HelperBackend::new(helper.path.clone())
            .list_windows(None)
            .unwrap();
        assert!(windows.is_empty());
    }

    #[test]
    fn error_envelope_maps_to_backend_error_kind() {
        let helper = fake_helper(
            "perm",
            r#"cat >/dev/null; printf '{"ok":false,"code":"permission_denied","error":"nope"}'"#,
        );
        let err = HelperBackend::new(helper.path.clone())
            .list_windows(None)
            .unwrap_err();
        assert_eq!(err.kind, BackendErrorKind::PermissionDenied);
    }

    #[test]
    fn yielded_envelope_maps_to_the_yielded_kind() {
        let helper = fake_helper(
            "yield",
            r#"cat >/dev/null; printf '{"ok":false,"code":"yielded","error":"a system security dialog is in the foreground"}'"#,
        );
        let err = HelperBackend::new(helper.path.clone())
            .list_windows(None)
            .unwrap_err();
        assert_eq!(err.kind, BackendErrorKind::Yielded);
    }

    #[test]
    fn unsupported_envelope_maps_to_the_unsupported_kind() {
        let helper = fake_helper(
            "unsupported",
            r#"cat >/dev/null; printf '{"ok":false,"code":"unsupported","error":"computer use requires macOS 14"}'"#,
        );
        let err = HelperBackend::new(helper.path.clone())
            .list_windows(None)
            .unwrap_err();
        assert_eq!(err.kind, BackendErrorKind::Unsupported);
    }

    #[test]
    fn launch_and_condition_results_parse_from_the_helper_envelope() {
        let helper = fake_helper(
            "launch",
            r#"cat >/dev/null; printf '{"ok":true,"result":{"success":true,"used_fallback":false,"detail":"launched"}}'"#,
        );
        let meta = HelperBackend::new(helper.path.clone())
            .launch_app("com.example.app", ExecutionMode::default())
            .unwrap();
        assert!(meta.success);
        // A helper that predates the mode contract reports no mode; nothing
        // is fabricated on its behalf.
        assert_eq!(meta.execution_mode, None);

        let helper = fake_helper(
            "wait",
            r#"cat >/dev/null; printf '{"ok":true,"result":{"met":true,"timed_out":false}}'"#,
        );
        let observation = HelperBackend::new(helper.path.clone())
            .wait_condition(
                "com.example.app",
                &WaitCondition::TextPresent {
                    text: "Ready".to_owned(),
                },
                12.5,
            )
            .unwrap();
        assert!(observation.met);
        assert!(!observation.timed_out);
    }

    #[test]
    fn control_requests_carry_the_execution_mode_to_the_helper() {
        let staging = tempfile::tempdir().unwrap();
        let request_path = staging.path().join("request.json");
        let helper = fake_helper(
            "mode",
            &format!(
                r#"cat > "{}"; printf '{{"ok":true,"result":{{"success":true,"used_fallback":false,"detail":null,"execution_mode":"background"}}}}'"#,
                request_path.display()
            ),
        );
        let backend = HelperBackend::new(helper.path.clone());

        // The default mode is background, spelled out explicitly on the wire.
        let meta = backend
            .click(
                "com.example.app",
                &ElementTarget::default(),
                None,
                None,
                ExecutionMode::default(),
            )
            .unwrap();
        let request: Value =
            serde_json::from_slice(&std::fs::read(&request_path).unwrap()).unwrap();
        assert_eq!(request["op"], "click");
        assert_eq!(request["execution_mode"], "background");
        // The helper's reported mode is parsed as truthful result metadata.
        assert_eq!(meta.execution_mode, Some(ExecutionMode::Background));

        // An approved foreground request reaches the helper as such.
        backend
            .focus_window("com.example.app", None, ExecutionMode::Foreground)
            .unwrap();
        let request: Value =
            serde_json::from_slice(&std::fs::read(&request_path).unwrap()).unwrap();
        assert_eq!(request["op"], "focus_window");
        assert_eq!(request["execution_mode"], "foreground");
    }

    #[test]
    fn requires_foreground_envelope_maps_to_its_kind() {
        let helper = fake_helper(
            "requires-fg",
            r#"cat >/dev/null; printf '{"ok":false,"code":"requires_foreground","error":"cannot preserve the pointer"}'"#,
        );
        let err = HelperBackend::new(helper.path.clone())
            .drag(
                "com.example.app",
                &ElementTarget::default(),
                &ElementTarget::default(),
                None,
                ExecutionMode::Background,
            )
            .unwrap_err();
        assert_eq!(err.kind, BackendErrorKind::RequiresForeground);
    }

    #[test]
    fn a_hung_helper_is_killed_at_the_timeout() {
        // Sleeps far past the timeout; must be killed and reported promptly,
        // not block the broker.
        let helper = fake_helper("hang", "sleep 30");
        let backend = HelperBackend::with_timeout(helper.path.clone(), Duration::from_millis(150));
        let start = Instant::now();
        let err = backend.list_windows(None).unwrap_err();
        assert_eq!(err.kind, BackendErrorKind::OperationFailed);
        assert!(err.message.contains("timed out"), "got: {}", err.message);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "should return promptly after killing, took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn empty_output_is_a_clean_error() {
        let helper = fake_helper("empty", "cat >/dev/null; exit 0");
        let err = HelperBackend::new(helper.path.clone())
            .list_windows(None)
            .unwrap_err();
        assert_eq!(err.kind, BackendErrorKind::OperationFailed);
        assert!(err.message.contains("no output"), "got: {}", err.message);
    }

    #[test]
    fn large_output_is_drained_without_deadlocking() {
        // ~200 KB of output exceeds the OS pipe buffer; the concurrent drain
        // must keep the child from blocking on a full pipe.
        let helper = fake_helper(
            "big",
            r#"cat >/dev/null; printf '{"ok":true,"result":"'; head -c 200000 /dev/zero | tr '\0' a; printf '"}'"#,
        );
        let result = HelperBackend::new(helper.path.clone())
            .run(serde_json::json!({ "op": "noop" }))
            .unwrap();
        assert!(result.is_string());
        assert_eq!(result.as_str().unwrap().len(), 200_000);
    }
}
