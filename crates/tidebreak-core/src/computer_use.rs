//! Model-facing contracts for computer use: screen capture and consent-gated
//! control of native macOS applications.
//!
//! These are client-executed tool contracts (schemas and typed arguments only —
//! no server executor). The calls are claimed and fulfilled by the desktop
//! client, which authorizes against the host broker's per-app capability grants
//! and performs the work on the host where the display and input devices live.
//! Sandboxed and background agents never hold these tools: they run where there
//! is no display.
//!
//! Targeting is accessibility-first. An element is addressed by its `mark` (a
//! Set-of-Marks number from the most recent annotated screenshot) or by
//! `element_id` + `element_fingerprint` (the index-path id and drift-detection
//! hash from a prior `computer_read_app_content`). Raw `x`/`y` coordinates are
//! a documented last resort for apps with no usable accessibility surface. The
//! desktop resolves a `mark` to its element before acting, and the helper
//! re-checks the fingerprint at act time so a shifted UI refuses as
//! `stale_element` rather than clicking whatever is now under the pointer.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ToolSpec;

/// List the on-screen windows, optionally for one app.
pub const COMPUTER_LIST_WINDOWS_TOOL: &str = "computer_list_windows";
/// Capture the screen (whole display or one app) as an image.
pub const COMPUTER_CAPTURE_SCREEN_TOOL: &str = "computer_capture_screen";
/// Read an app's accessibility tree (on-screen text, element roles, bounds).
pub const COMPUTER_READ_APP_CONTENT_TOOL: &str = "computer_read_app_content";
/// Click an element or point in an app.
pub const COMPUTER_CLICK_TOOL: &str = "computer_click";
/// Type text into an element or the focused field.
pub const COMPUTER_TYPE_TEXT_TOOL: &str = "computer_type_text";
/// Press a key (optionally a chord) in the focused app.
pub const COMPUTER_KEY_PRESS_TOOL: &str = "computer_key_press";
/// Scroll an element or point by a pixel delta.
pub const COMPUTER_SCROLL_TOOL: &str = "computer_scroll";
/// Bring an app (or one of its windows) to the front.
pub const COMPUTER_FOCUS_WINDOW_TOOL: &str = "computer_focus_window";
/// Return focus to the Tidebreak window (e.g. to read the transcript).
pub const COMPUTER_RETURN_TO_TIDEBREAK_TOOL: &str = "computer_return_to_tidebreak";
/// Wait a bounded number of seconds (e.g. for an app to finish an action).
pub const COMPUTER_WAIT_TOOL: &str = "computer_wait";
/// Launch an app by its approved bundle id.
pub const COMPUTER_LAUNCH_APP_TOOL: &str = "computer_launch_app";
/// Move the pointer over an element or point in an app without pressing.
pub const COMPUTER_HOVER_TOOL: &str = "computer_hover";
/// Press and drag from one element/point to another within an app.
pub const COMPUTER_DRAG_TOOL: &str = "computer_drag";
/// Resize one window of an app in logical points.
pub const COMPUTER_RESIZE_WINDOW_TOOL: &str = "computer_resize_window";

/// Longest text `computer_type_text` will enter in one call. The helper's
/// synthesized-keystroke fallback is further bounded (it would otherwise risk a
/// mid-type timeout); the atomic value-set path is unbounded up to this cap.
pub const MAX_TYPE_TEXT_CHARS: usize = 10_000;
/// Highest Set-of-Marks number a call may reference (marks are 1-based).
pub const MAX_MARK: u32 = 80;
/// Hard cap on the AX-tree read bounds, mirrored from the helper.
pub const MAX_READ_DEPTH: u32 = 25;
pub const MAX_READ_NODES: u32 = 2000;
/// Longest `computer_wait` sleep, in seconds.
pub const MAX_WAIT_SECONDS: f64 = 10.0;
/// Longest model-requested drag, in milliseconds. The helper enforces the
/// same bound, so a drag is always finite and cancellable.
pub const MAX_DRAG_DURATION_MS: u64 = 10_000;
/// Longest condition text a wait may request, matching the browser wait
/// surface.
pub const MAX_WAIT_CONDITION_TEXT_CHARS: usize = 512;
/// Upper bound on one requested window dimension (logical points). Keeps a
/// nonsense request from reaching a window server resize.
pub const MAX_WINDOW_DIMENSION: f64 = 10_000.0;
/// Default long-edge cap applied to native captures when the model does not
/// ask for one. Keeps PNGs near the MCP transport budget while staying
/// readable.
pub const DEFAULT_CAPTURE_MAX_DIMENSION: u32 = 1440;
/// Hard cap a capture may request, applied again by the helper so a buggy
/// caller cannot push an unbounded pixel buffer.
pub const MAX_CAPTURE_MAX_DIMENSION: u32 = 4096;
/// Default total time a condition wait polls, in seconds.
pub const DEFAULT_CONDITION_TIMEOUT_SECONDS: f64 = 10.0;
/// Hard bound a condition wait may request.
pub const MAX_CONDITION_TIMEOUT_SECONDS: f64 = 30.0;

/// All fourteen computer-use tool names.
pub const COMPUTER_USE_TOOLS: [&str; 14] = [
    COMPUTER_LIST_WINDOWS_TOOL,
    COMPUTER_CAPTURE_SCREEN_TOOL,
    COMPUTER_READ_APP_CONTENT_TOOL,
    COMPUTER_CLICK_TOOL,
    COMPUTER_TYPE_TEXT_TOOL,
    COMPUTER_KEY_PRESS_TOOL,
    COMPUTER_SCROLL_TOOL,
    COMPUTER_FOCUS_WINDOW_TOOL,
    COMPUTER_RETURN_TO_TIDEBREAK_TOOL,
    COMPUTER_WAIT_TOOL,
    COMPUTER_LAUNCH_APP_TOOL,
    COMPUTER_HOVER_TOOL,
    COMPUTER_DRAG_TOOL,
    COMPUTER_RESIZE_WINDOW_TOOL,
];

/// The control (acting) tools, which require the `ControlApp` grant and gate
/// behind the `ComputerMayControlApp` approval kind. Launch, hover, drag, and
/// resize all mutate the user's real host state or synthesize input, so they
/// are control tools alongside click/type/key/scroll/focus. Reads never card
/// per-call once their grant exists.
pub const COMPUTER_USE_CONTROL_TOOLS: [&str; 9] = [
    COMPUTER_CLICK_TOOL,
    COMPUTER_TYPE_TEXT_TOOL,
    COMPUTER_KEY_PRESS_TOOL,
    COMPUTER_SCROLL_TOOL,
    COMPUTER_FOCUS_WINDOW_TOOL,
    COMPUTER_LAUNCH_APP_TOOL,
    COMPUTER_HOVER_TOOL,
    COMPUTER_DRAG_TOOL,
    COMPUTER_RESIZE_WINDOW_TOOL,
];

/// Whether `name` is any computer-use tool.
#[must_use]
pub fn is_computer_use_tool(name: &str) -> bool {
    COMPUTER_USE_TOOLS.contains(&name)
}

/// Whether `name` is a computer-use control tool (click / type / key).
#[must_use]
pub fn is_computer_use_control_tool(name: &str) -> bool {
    COMPUTER_USE_CONTROL_TOOLS.contains(&name)
}

/// Shared guidance folded into the acting tools' descriptions: computer use is
/// primarily an observation surface, while GUI driving is a disruptive fallback.
const ACTING_NOTE: &str = "\n\nUse GUI control sparingly. Clicking, typing, scrolling, and moving focus use the user's real interface and are slower, more brittle, and more disruptive than reading app content or using a dedicated tool. Read first, prefer a non-GUI path when one exists, and act only when it is necessary to complete the user's request. The user can stop control at any time.\n\nActions run in the default `background` execution mode: the host drives the app directly while preserving the user's focus, pointer, and active window. Set `execution_mode` to \"foreground\" only when an action genuinely needs the real pointer or focus; foreground control asks the user for a separate takeover permission first. An action that cannot be performed without taking over refuses with `requires_foreground` — it is never retried or escalated automatically.";

/// Shared targeting guidance: prefer a Set-of-Marks number or an element
/// identity over raw coordinates.
const TARGETING_NOTE: &str = "Target by `mark` (a number from the last annotated screenshot) or by `element_id` + `element_fingerprint` from `computer_read_app_content`. Use `x`/`y` coordinates only when the app exposes no usable accessibility element.";

/// How a control action interacts with the user's live session. Background is
/// the default everywhere: the host acts on the app directly and must leave
/// the user's focus, pointer, and active window untouched. Foreground is an
/// explicit takeover the desktop only honors after a separate trusted native
/// approval per app and chat — an app-control grant alone never implies it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "", transform = crate::client_tools::preserve_enum_wire_shape)]
pub enum ExecutionMode {
    /// Act without disturbing the user's focus or pointer (the default). An
    /// action that cannot honor this refuses with `requires_foreground`.
    #[default]
    #[schemars(description = "")]
    Background,
    /// Take over the real pointer/focus. Requires a separate user approval.
    #[schemars(description = "")]
    Foreground,
}

/// Which mouse button a click uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "", transform = crate::client_tools::preserve_enum_wire_shape)]
pub enum ClickButton {
    /// Primary (left) button — the default.
    #[schemars(description = "")]
    Left,
    /// Secondary (right) button, for context menus.
    #[schemars(description = "")]
    Right,
}

/// A chord modifier for `computer_key_press`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "", transform = crate::client_tools::preserve_enum_wire_shape)]
pub enum KeyModifier {
    #[schemars(description = "")]
    Cmd,
    #[schemars(description = "")]
    Shift,
    #[schemars(description = "")]
    Ctrl,
    #[schemars(description = "")]
    Alt,
    #[schemars(description = "")]
    Fn,
}

// MARK: - Read tools

/// Canonical arguments for [`COMPUTER_LIST_WINDOWS_TOOL`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerListWindowsArgs {
    /// Restrict to one app by its bundle id (e.g. "com.apple.Notes"). Omit to
    /// list every on-screen window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Optional app bundle id to filter by.")]
    pub app_id: Option<String>,
}

/// Canonical arguments for [`COMPUTER_CAPTURE_SCREEN_TOOL`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerCaptureScreenArgs {
    /// Capture only this app's windows (by bundle id). Omit to capture the
    /// whole display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Optional app bundle id to capture; omit for the whole screen.")]
    pub app_id: Option<String>,
    /// Which display to capture (a CGDirectDisplayID). Omit for the main
    /// display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Optional display id; omit for the main display.")]
    pub display_id: Option<u32>,
    /// Draw numbered Set-of-Marks badges over interactive elements so you can
    /// act on them by number. Defaults to true.
    #[serde(default = "default_true")]
    #[schemars(description = "Annotate interactive elements with numbered marks.")]
    pub annotate: bool,
    /// Select one window of the app (from `computer_list_windows`) instead of
    /// every window of the app. Requires `app_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Optional window id to capture; app capture only.")]
    pub window_id: Option<u32>,
    /// Cap the longest image edge in pixels after capture so the image fits
    /// the transport budget (default 1440, max 4096). The image is downscaled
    /// to this edge. Use the returned coordinate frame to map screenshot
    /// pixels into the global logical coordinates accepted by input tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        range(min = 1, max = MAX_CAPTURE_MAX_DIMENSION),
        description = "Max image edge in pixels (default 1440, max 4096)."
    )]
    pub max_dimension: Option<u32>,
}

fn default_true() -> bool {
    true
}

fn window_dimension_ok(value: Option<f64>) -> bool {
    value.is_none_or(|v| v.is_finite() && v > 0.0 && v <= MAX_WINDOW_DIMENSION)
}

/// Canonical arguments for [`COMPUTER_READ_APP_CONTENT_TOOL`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerReadAppContentArgs {
    /// The app to read, by bundle id.
    #[schemars(description = "App bundle id (e.g. \"com.apple.Notes\").")]
    pub app_id: String,
    /// Maximum accessibility-tree depth (defaults to 12, capped at 25).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Max tree depth (default 12, max 25).")]
    pub max_depth: Option<u32>,
    /// Maximum number of tree nodes (defaults to 500, capped at 2000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Max node count (default 500, max 2000).")]
    pub max_nodes: Option<u32>,
}

/// The shared element/coordinate target carried by the acting and scroll tools.
/// All fields are optional; an empty target means the focused app/field.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ElementTargetArgs {
    /// A Set-of-Marks number from the last annotated screenshot. The desktop
    /// resolves it to the underlying element before acting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Set-of-Marks number from the last annotated screenshot (1-80).")]
    pub mark: Option<u32>,
    /// The element's index-path id from `computer_read_app_content`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Element index-path id from computer_read_app_content.")]
    pub element_id: Option<String>,
    /// The element's fingerprint, re-checked at act time to detect drift.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Element fingerprint from computer_read_app_content.")]
    pub element_fingerprint: Option<String>,
    /// Raw coordinate fallback (global, top-left origin). Last resort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Raw X coordinate; last resort when no element is usable.")]
    pub x: Option<f64>,
    /// Raw coordinate fallback (global, top-left origin). Last resort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Raw Y coordinate; last resort when no element is usable.")]
    pub y: Option<f64>,
}

/// Canonical arguments for [`COMPUTER_CLICK_TOOL`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerClickArgs {
    /// The app to click in, by bundle id.
    #[schemars(description = "App bundle id.")]
    pub app_id: String,
    /// What to click.
    #[serde(flatten)]
    pub target: ElementTargetArgs,
    /// Which button. Defaults to left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "ClickButton", description = "Mouse button (default left).")]
    pub button: Option<ClickButton>,
    /// Double-click when true. Defaults to a single click.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Double-click when true (default single).")]
    pub double: Option<bool>,
    /// Background (default) preserves the user's focus and pointer;
    /// foreground takes over and needs the user's separate approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        with = "ExecutionMode",
        description = "Execution mode: \"background\" (default) preserves the user's focus and pointer; \"foreground\" takes over and requires separate user approval."
    )]
    pub execution_mode: Option<ExecutionMode>,
}

/// Canonical arguments for [`COMPUTER_TYPE_TEXT_TOOL`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerTypeTextArgs {
    /// The app to type into, by bundle id.
    #[schemars(description = "App bundle id.")]
    pub app_id: String,
    /// The text to enter.
    #[schemars(
        length(min = 1, max = MAX_TYPE_TEXT_CHARS),
        description = "Text to type."
    )]
    pub text: String,
    /// Where to type. Omit to type into the focused field.
    #[serde(flatten)]
    pub target: ElementTargetArgs,
    /// Background (default) preserves the user's focus and pointer;
    /// foreground takes over and needs the user's separate approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        with = "ExecutionMode",
        description = "Execution mode: \"background\" (default) preserves the user's focus and pointer; \"foreground\" takes over and requires separate user approval."
    )]
    pub execution_mode: Option<ExecutionMode>,
}

/// Canonical arguments for [`COMPUTER_KEY_PRESS_TOOL`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerKeyPressArgs {
    /// The app to send the key to, by bundle id.
    #[schemars(description = "App bundle id.")]
    pub app_id: String,
    /// The key name (e.g. "return", "tab", "escape", "a", "left").
    #[schemars(
        length(min = 1),
        description = "Key name (e.g. \"return\", \"tab\", \"a\")."
    )]
    pub key: String,
    /// Chord modifiers to hold while pressing the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Chord modifiers to hold (cmd/shift/ctrl/alt/fn).")]
    pub modifiers: Option<Vec<KeyModifier>>,
    /// Background (default) preserves the user's focus and pointer;
    /// foreground takes over and needs the user's separate approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        with = "ExecutionMode",
        description = "Execution mode: \"background\" (default) preserves the user's focus and pointer; \"foreground\" takes over and requires separate user approval."
    )]
    pub execution_mode: Option<ExecutionMode>,
}

/// Canonical arguments for [`COMPUTER_SCROLL_TOOL`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerScrollArgs {
    /// The app to scroll in, by bundle id.
    #[schemars(description = "App bundle id.")]
    pub app_id: String,
    /// Where to scroll. Omit to scroll at the current pointer location.
    #[serde(flatten)]
    pub target: ElementTargetArgs,
    /// Horizontal pixel delta (positive scrolls right).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Horizontal pixel delta (positive = right).")]
    pub dx: Option<f64>,
    /// Vertical pixel delta (positive scrolls down).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Vertical pixel delta (positive = down).")]
    pub dy: Option<f64>,
    /// Background (default) preserves the user's focus and pointer;
    /// foreground takes over and needs the user's separate approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        with = "ExecutionMode",
        description = "Execution mode: \"background\" (default) preserves the user's focus and pointer; \"foreground\" takes over and requires separate user approval."
    )]
    pub execution_mode: Option<ExecutionMode>,
}

/// Canonical arguments for [`COMPUTER_FOCUS_WINDOW_TOOL`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerFocusWindowArgs {
    /// The app to bring to the front, by bundle id.
    #[schemars(description = "App bundle id.")]
    pub app_id: String,
    /// A specific window to raise. Omit to focus the app's main window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Optional window id to raise.")]
    pub window_id: Option<u32>,
    /// Focusing always changes which app the user is looking at, so this tool
    /// only acts in foreground mode with the user's separate approval. The
    /// background default refuses with `requires_foreground`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        with = "ExecutionMode",
        description = "Execution mode. Focusing takes over the user's screen, so this tool requires \"foreground\" (with separate user approval); the \"background\" default refuses."
    )]
    pub execution_mode: Option<ExecutionMode>,
}

/// Canonical arguments for [`COMPUTER_RETURN_TO_TIDEBREAK_TOOL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerReturnToTidebreakArgs {
    /// Raising the Tidebreak window steals whatever the user is focused on,
    /// so this tool only acts in foreground mode with the user's separate
    /// approval. The background default refuses with `requires_foreground`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        with = "ExecutionMode",
        description = "Execution mode. Raising the Tidebreak window takes over the user's focus, so this tool requires \"foreground\" (with separate user approval); the \"background\" default refuses."
    )]
    pub execution_mode: Option<ExecutionMode>,
}

/// Canonical arguments for [`COMPUTER_WAIT_TOOL`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerWaitArgs {
    /// App to observe. Required with a condition; omitted for a fixed pause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "App bundle id, required when waiting for a condition.")]
    pub app_id: Option<String>,
    /// How long to wait, in seconds (default 1, max 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Seconds to wait (default 1, max 10).")]
    pub seconds: Option<f64>,
    /// Optional deterministic condition to wait for instead of a fixed pause.
    /// App/window/text conditions poll the live app state; they never type or
    /// click and never fall back to a fixed sleep when the condition fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Optional condition to wait for.")]
    pub condition: Option<ComputerWaitConditionArgs>,
    /// Total time to poll a `condition`, in seconds (default 10, max 30).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        range(min = 0.1, max = MAX_CONDITION_TIMEOUT_SECONDS),
        description = "Maximum condition wait in seconds (default 10, max 30)."
    )]
    pub condition_timeout_seconds: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ComputerWaitConditionArgs {
    /// Wait until a process with `app_id` is a running application.
    AppRunning,
    /// Wait until `app_id` has at least one on-screen window.
    WindowVisible,
    /// Wait until the app's accessibility tree contains `text` exactly (case
    /// sensitive, matching an element title/description/value).
    TextPresent { text: String },
    /// Wait until the app's accessibility tree no longer contains `text`
    /// exactly (case sensitive, matching an element title/description/value).
    TextAbsent { text: String },
}

/// Canonical arguments for [`COMPUTER_LAUNCH_APP_TOOL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerLaunchAppArgs {
    /// The app to launch, by bundle id. The helper only opens the registered
    /// application the id resolves to through NSWorkspace; arbitrary
    /// executables, paths, and arguments are never accepted.
    #[schemars(
        length(min = 1, max = 256),
        description = "App bundle id to launch (e.g. \"com.apple.Notes\")."
    )]
    pub app_id: String,
    /// Background (default) launches without activating the app over the
    /// user's current focus; foreground activation needs separate approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        with = "ExecutionMode",
        description = "Execution mode: \"background\" (default) launches without stealing the user's focus; \"foreground\" activates the app and requires separate user approval."
    )]
    pub execution_mode: Option<ExecutionMode>,
}

/// Canonical arguments for [`COMPUTER_HOVER_TOOL`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerHoverArgs {
    /// The app to hover in, by bundle id.
    #[schemars(description = "App bundle id.")]
    pub app_id: String,
    /// The element or point to hover. Raw coordinates are global and are
    /// re-validated against the app's on-screen windows immediately before
    /// the pointer moves.
    #[serde(flatten)]
    pub target: ElementTargetArgs,
    /// Background (default) preserves the user's focus and pointer;
    /// foreground takes over and needs the user's separate approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        with = "ExecutionMode",
        description = "Execution mode: \"background\" (default) preserves the user's focus and pointer; \"foreground\" takes over and requires separate user approval."
    )]
    pub execution_mode: Option<ExecutionMode>,
}

/// Canonical arguments for [`COMPUTER_DRAG_TOOL`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerDragArgs {
    /// The app to drag in, by bundle id.
    #[schemars(description = "App bundle id.")]
    pub app_id: String,
    /// Where the press goes down.
    #[schemars(description = "Drag origin: element or point.")]
    pub from: ElementTargetArgs,
    /// Where the button releases.
    #[schemars(description = "Drag destination: element or point.")]
    pub to: ElementTargetArgs,
    /// Duration of the drag in milliseconds (default 200, max 10000). Both
    /// endpoints are validated before the first mouse-down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        range(min = 0, max = MAX_DRAG_DURATION_MS),
        description = "Drag duration in ms (default 200, max 10000)."
    )]
    pub duration_ms: Option<u64>,
    /// Background (default) preserves the user's focus and pointer;
    /// foreground takes over and needs the user's separate approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        with = "ExecutionMode",
        description = "Execution mode: \"background\" (default) preserves the user's focus and pointer; \"foreground\" takes over and requires separate user approval."
    )]
    pub execution_mode: Option<ExecutionMode>,
}

/// Canonical arguments for [`COMPUTER_RESIZE_WINDOW_TOOL`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerResizeWindowArgs {
    /// The app owning the window, by bundle id.
    #[schemars(description = "App bundle id.")]
    pub app_id: String,
    /// Which window to resize (from `computer_list_windows`). Omit for the
    /// app's main/frontmost window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Optional window id to resize.")]
    pub window_id: Option<u32>,
    /// New width in logical points (max 10000).
    #[schemars(range(min = 1.0, max = MAX_WINDOW_DIMENSION))]
    pub width: f64,
    /// New height in logical points (max 10000).
    #[schemars(range(min = 1.0, max = MAX_WINDOW_DIMENSION))]
    pub height: f64,
    /// Background (default) preserves the user's focus and pointer;
    /// foreground takes over and needs the user's separate approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        with = "ExecutionMode",
        description = "Execution mode: \"background\" (default) preserves the user's focus and pointer; \"foreground\" takes over and requires separate user approval."
    )]
    pub execution_mode: Option<ExecutionMode>,
}

// MARK: - Validation

fn parse<T: for<'de> Deserialize<'de>>(arguments: &Value) -> Option<T> {
    serde_json::from_value::<T>(arguments.clone()).ok()
}

/// Whether a shared target is internally consistent: a `mark` stays in range,
/// and a partial coordinate (only one of `x`/`y`) is rejected.
fn target_is_well_formed(target: &ElementTargetArgs) -> bool {
    if let Some(mark) = target.mark {
        if mark == 0 || mark > MAX_MARK {
            return false;
        }
    }
    // A coordinate target needs both axes.
    if target.x.is_some() != target.y.is_some() {
        return false;
    }
    true
}

macro_rules! validate_fn {
    ($name:ident, $args:ty) => {
        /// Validate one canonical JSON payload before it crosses the trusted-client boundary.
        #[must_use]
        pub fn $name(arguments: &Value) -> bool {
            parse::<$args>(arguments).is_some()
        }
    };
}

validate_fn!(
    validate_computer_list_windows_arguments,
    ComputerListWindowsArgs
);
validate_fn!(
    validate_computer_focus_window_arguments,
    ComputerFocusWindowArgs
);
validate_fn!(
    validate_computer_return_to_tidebreak_arguments,
    ComputerReturnToTidebreakArgs
);

/// Validate a `computer_capture_screen` payload.
#[must_use]
pub fn validate_computer_capture_screen_arguments(arguments: &Value) -> bool {
    let Some(args) = parse::<ComputerCaptureScreenArgs>(arguments) else {
        return false;
    };
    args.max_dimension
        .is_none_or(|edge| (1..=MAX_CAPTURE_MAX_DIMENSION).contains(&edge))
        && args.app_id.as_ref().is_none_or(|id| !id.trim().is_empty())
        && (args.window_id.is_none() || args.app_id.is_some())
        && !(args.app_id.is_some() && args.display_id.is_some())
}

/// Validate a `computer_read_app_content` payload, enforcing the read bounds.
#[must_use]
pub fn validate_computer_read_app_content_arguments(arguments: &Value) -> bool {
    let Some(args) = parse::<ComputerReadAppContentArgs>(arguments) else {
        return false;
    };
    if args.app_id.trim().is_empty() {
        return false;
    }
    args.max_depth
        .is_none_or(|d| (1..=MAX_READ_DEPTH).contains(&d))
        && args
            .max_nodes
            .is_none_or(|n| (1..=MAX_READ_NODES).contains(&n))
}

/// Validate a `computer_click` payload.
#[must_use]
pub fn validate_computer_click_arguments(arguments: &Value) -> bool {
    let Some(args) = parse::<ComputerClickArgs>(arguments) else {
        return false;
    };
    !args.app_id.trim().is_empty() && target_is_well_formed(&args.target)
}

/// Validate a `computer_type_text` payload, enforcing the text bound.
#[must_use]
pub fn validate_computer_type_text_arguments(arguments: &Value) -> bool {
    let Some(args) = parse::<ComputerTypeTextArgs>(arguments) else {
        return false;
    };
    !args.app_id.trim().is_empty()
        && !args.text.is_empty()
        && args.text.chars().count() <= MAX_TYPE_TEXT_CHARS
        && target_is_well_formed(&args.target)
}

/// Validate a `computer_key_press` payload.
#[must_use]
pub fn validate_computer_key_press_arguments(arguments: &Value) -> bool {
    let Some(args) = parse::<ComputerKeyPressArgs>(arguments) else {
        return false;
    };
    !args.app_id.trim().is_empty() && !args.key.trim().is_empty()
}

/// Validate a `computer_scroll` payload.
#[must_use]
pub fn validate_computer_scroll_arguments(arguments: &Value) -> bool {
    let Some(args) = parse::<ComputerScrollArgs>(arguments) else {
        return false;
    };
    !args.app_id.trim().is_empty() && target_is_well_formed(&args.target)
}

/// Validate a `computer_wait` payload, enforcing the sleep bound.
#[must_use]
pub fn validate_computer_wait_arguments(arguments: &Value) -> bool {
    let Some(args) = parse::<ComputerWaitArgs>(arguments) else {
        return false;
    };
    let seconds_ok = args
        .seconds
        .is_none_or(|s| s.is_finite() && (0.0..=MAX_WAIT_SECONDS).contains(&s));
    let timeout_ok = args
        .condition_timeout_seconds
        .is_none_or(|s| s.is_finite() && (0.1..=MAX_CONDITION_TIMEOUT_SECONDS).contains(&s));
    let condition_ok = match &args.condition {
        None => true,
        Some(ComputerWaitConditionArgs::AppRunning)
        | Some(ComputerWaitConditionArgs::WindowVisible) => true,
        Some(ComputerWaitConditionArgs::TextPresent { text })
        | Some(ComputerWaitConditionArgs::TextAbsent { text }) => {
            !text.trim().is_empty() && text.chars().count() <= MAX_WAIT_CONDITION_TEXT_CHARS
        }
    };
    let scope_ok = if args.condition.is_some() {
        args.app_id.as_ref().is_some_and(|id| !id.trim().is_empty()) && args.seconds.is_none()
    } else {
        args.app_id.is_none() && args.condition_timeout_seconds.is_none()
    };
    seconds_ok && timeout_ok && condition_ok && scope_ok
}

/// Validate a `computer_launch_app` payload.
#[must_use]
pub fn validate_computer_launch_app_arguments(arguments: &Value) -> bool {
    let Some(args) = parse::<ComputerLaunchAppArgs>(arguments) else {
        return false;
    };
    !args.app_id.trim().is_empty()
}

/// Validate a `computer_hover` payload.
#[must_use]
pub fn validate_computer_hover_arguments(arguments: &Value) -> bool {
    let Some(args) = parse::<ComputerHoverArgs>(arguments) else {
        return false;
    };
    !args.app_id.trim().is_empty() && target_is_well_formed(&args.target)
}

/// Validate a `computer_drag` payload, enforcing the duration bound and the
/// shape of both endpoints.
#[must_use]
pub fn validate_computer_drag_arguments(arguments: &Value) -> bool {
    let Some(args) = parse::<ComputerDragArgs>(arguments) else {
        return false;
    };
    !args.app_id.trim().is_empty()
        && target_is_well_formed(&args.from)
        && target_is_well_formed(&args.to)
        && args.duration_ms.is_none_or(|ms| ms <= MAX_DRAG_DURATION_MS)
}

/// Validate a `computer_resize_window` payload, enforcing the window bounds.
#[must_use]
pub fn validate_computer_resize_window_arguments(arguments: &Value) -> bool {
    let Some(args) = parse::<ComputerResizeWindowArgs>(arguments) else {
        return false;
    };
    !args.app_id.trim().is_empty()
        && window_dimension_ok(Some(args.width))
        && window_dimension_ok(Some(args.height))
}

/// Return the canonical native computer-use surface for every agent transport.
/// The host still authorizes each call against its session and app grants.
#[must_use]
pub fn computer_use_tool_specs() -> Vec<ToolSpec> {
    vec![
        computer_list_windows_tool_spec(),
        computer_capture_screen_tool_spec(),
        computer_read_app_content_tool_spec(),
        computer_click_tool_spec(),
        computer_type_text_tool_spec(),
        computer_key_press_tool_spec(),
        computer_scroll_tool_spec(),
        computer_focus_window_tool_spec(),
        computer_return_to_tidebreak_tool_spec(),
        computer_wait_tool_spec(),
        computer_launch_app_tool_spec(),
        computer_hover_tool_spec(),
        computer_drag_tool_spec(),
        computer_resize_window_tool_spec(),
    ]
}

/// Validate a named call before a transport forwards it to the native host.
/// Unknown tools never reach the host executor.
#[must_use]
pub fn validate_computer_use_arguments(name: &str, arguments: &Value) -> bool {
    match name {
        COMPUTER_LIST_WINDOWS_TOOL => validate_computer_list_windows_arguments(arguments),
        COMPUTER_CAPTURE_SCREEN_TOOL => validate_computer_capture_screen_arguments(arguments),
        COMPUTER_READ_APP_CONTENT_TOOL => validate_computer_read_app_content_arguments(arguments),
        COMPUTER_CLICK_TOOL => validate_computer_click_arguments(arguments),
        COMPUTER_TYPE_TEXT_TOOL => validate_computer_type_text_arguments(arguments),
        COMPUTER_KEY_PRESS_TOOL => validate_computer_key_press_arguments(arguments),
        COMPUTER_SCROLL_TOOL => validate_computer_scroll_arguments(arguments),
        COMPUTER_FOCUS_WINDOW_TOOL => validate_computer_focus_window_arguments(arguments),
        COMPUTER_RETURN_TO_TIDEBREAK_TOOL => {
            validate_computer_return_to_tidebreak_arguments(arguments)
        }
        COMPUTER_WAIT_TOOL => validate_computer_wait_arguments(arguments),
        COMPUTER_LAUNCH_APP_TOOL => validate_computer_launch_app_arguments(arguments),
        COMPUTER_HOVER_TOOL => validate_computer_hover_arguments(arguments),
        COMPUTER_DRAG_TOOL => validate_computer_drag_arguments(arguments),
        COMPUTER_RESIZE_WINDOW_TOOL => validate_computer_resize_window_arguments(arguments),
        _ => false,
    }
}

// MARK: - Tool specs

/// Tool contract for [`COMPUTER_LIST_WINDOWS_TOOL`].
#[must_use]
pub fn computer_list_windows_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerListWindowsArgs>(
        COMPUTER_LIST_WINDOWS_TOOL,
        "List the on-screen windows, optionally for one app. Use this first when you do not know what is open or need an app's bundle id before reading or capturing it. Window ids are ephemeral, so use them promptly.",
    )
}

/// Tool contract for [`COMPUTER_CAPTURE_SCREEN_TOOL`].
#[must_use]
pub fn computer_capture_screen_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerCaptureScreenArgs>(
        COMPUTER_CAPTURE_SCREEN_TOOL,
        "Capture the screen as an image — the whole display, or one app's windows. Use this for visual layout, charts, images, or custom-drawn interfaces that the accessibility tree cannot convey. Prefer computer_read_app_content when you only need text or structure. Annotated captures include numbered marks that acting tools can target without guessing coordinates.",
    )
}

/// Tool contract for [`COMPUTER_READ_APP_CONTENT_TOOL`].
#[must_use]
pub fn computer_read_app_content_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerReadAppContentArgs>(
        COMPUTER_READ_APP_CONTENT_TOOL,
        "Read an app's accessibility tree: on-screen text, element roles, values, and bounds. This is the primary way to see an app because structured content is cheaper and more precise than pixels, and it yields the marks, element ids, and fingerprints acting tools target. If the result is truncated, narrow max_depth or max_nodes; use a screenshot only when structure is incomplete or visual layout matters.",
    )
}

/// Tool contract for [`COMPUTER_CLICK_TOOL`].
#[must_use]
pub fn computer_click_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerClickArgs>(
        COMPUTER_CLICK_TOOL,
        &format!("Click an element or point in an app. {TARGETING_NOTE}{ACTING_NOTE}"),
    )
}

/// Tool contract for [`COMPUTER_TYPE_TEXT_TOOL`].
#[must_use]
pub fn computer_type_text_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerTypeTextArgs>(
        COMPUTER_TYPE_TEXT_TOOL,
        &format!(
            "Type text into an element or the focused field. {TARGETING_NOTE}{ACTING_NOTE}\n\nA newline in `text` is typed as the Return key, which submits many composers and forms instead of inserting a line break — keep `text` to a single line unless you intend to submit, or use Shift+Return via `computer_key_press` where the app supports it."
        ),
    )
}

/// Tool contract for [`COMPUTER_KEY_PRESS_TOOL`].
#[must_use]
pub fn computer_key_press_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerKeyPressArgs>(
        COMPUTER_KEY_PRESS_TOOL,
        &format!(
            "Press a key, optionally with chord modifiers, in the focused app. Use for keyboard shortcuts and navigation keys. Key presses deliver real keystrokes, so in background mode they usually refuse with `requires_foreground` — prefer `computer_type_text`, which can set a value without focus, or use foreground mode when a real shortcut is unavoidable.{ACTING_NOTE}"
        ),
    )
}

/// Tool contract for [`COMPUTER_SCROLL_TOOL`].
#[must_use]
pub fn computer_scroll_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerScrollArgs>(
        COMPUTER_SCROLL_TOOL,
        &format!("Scroll an element or point by a pixel delta. Use it to reveal content before reading or targeting it. {TARGETING_NOTE}{ACTING_NOTE}"),
    )
}

/// Tool contract for [`COMPUTER_FOCUS_WINDOW_TOOL`].
#[must_use]
pub fn computer_focus_window_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerFocusWindowArgs>(
        COMPUTER_FOCUS_WINDOW_TOOL,
        &format!("Bring an app (or one of its windows) to the front. Focusing always takes over what the user is looking at, so this tool acts only with `execution_mode` set to \"foreground\" and the user's separate foreground approval; the background default refuses with `requires_foreground`. Background control does not need focus — act on the app directly instead.{ACTING_NOTE}"),
    )
}

/// Tool contract for [`COMPUTER_RETURN_TO_TIDEBREAK_TOOL`].
#[must_use]
pub fn computer_return_to_tidebreak_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerReturnToTidebreakArgs>(
        COMPUTER_RETURN_TO_TIDEBREAK_TOOL,
        "Return focus to the Tidebreak window. Raising Tidebreak takes over whatever the user is focused on, so this tool acts only with `execution_mode` set to \"foreground\" and the user's separate foreground approval; the background default refuses with `requires_foreground`. It is rarely needed — the user can switch back themselves.",
    )
}

/// Tool contract for [`COMPUTER_WAIT_TOOL`].
#[must_use]
pub fn computer_wait_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerWaitArgs>(
        COMPUTER_WAIT_TOOL,
        "Wait for an app to finish an action, appear, or reach a requested condition before the next read or capture. Without a condition, waits a bounded number of seconds. With a condition, polls the live app/window/accessibility state until it resolves or the bounded timeout expires; it never types, clicks, or falls back to a sleep when the condition is not met.",
    )
}

/// Tool contract for [`COMPUTER_LAUNCH_APP_TOOL`].
#[must_use]
pub fn computer_launch_app_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerLaunchAppArgs>(
        COMPUTER_LAUNCH_APP_TOOL,
        &format!("Launch an app by its registered bundle id (e.g. \"com.apple.Notes\"). Only the system's approved application identity is opened — arbitrary executables, paths, and command arguments are never accepted. Use before listing/reading an app that is not running.{ACTING_NOTE}"),
    )
}

/// Tool contract for [`COMPUTER_HOVER_TOOL`].
#[must_use]
pub fn computer_hover_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerHoverArgs>(
        COMPUTER_HOVER_TOOL,
        &format!("Move the pointer over an element or point in an app without pressing. Use to reveal hover menus, tooltips, or drag affordances before a read or drag. Hovering moves the user's real pointer, so in background mode it refuses with `requires_foreground`; it needs foreground mode and the user's takeover approval. {TARGETING_NOTE}{ACTING_NOTE}"),
    )
}

/// Tool contract for [`COMPUTER_DRAG_TOOL`].
#[must_use]
pub fn computer_drag_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerDragArgs>(
        COMPUTER_DRAG_TOOL,
        &format!("Press at the `from` element/point and release at the `to` element/point within one app. Use for sliders, reordering, selection ranges, and custom canvas interactions. Both endpoints are resolved and validated against the app before the first mouse-down, and the duration is bounded. Dragging moves the user's real pointer, so in background mode it refuses with `requires_foreground`; it needs foreground mode and the user's takeover approval. {TARGETING_NOTE}{ACTING_NOTE}"),
    )
}

/// Tool contract for [`COMPUTER_RESIZE_WINDOW_TOOL`].
#[must_use]
pub fn computer_resize_window_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerResizeWindowArgs>(
        COMPUTER_RESIZE_WINDOW_TOOL,
        &format!("Resize one window of an app to the given width and height in logical points. Use to arrange testable window sizes; logical points map to physical pixels through the display backing scale factor. {ACTING_NOTE}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shared_transport_surface_uses_canonical_specs_and_validation() {
        let specs = computer_use_tool_specs();
        assert_eq!(specs.len(), COMPUTER_USE_TOOLS.len());
        for (spec, name) in specs.iter().zip(COMPUTER_USE_TOOLS) {
            assert_eq!(spec.name, name);
        }
        assert!(validate_computer_use_arguments(
            COMPUTER_LIST_WINDOWS_TOOL,
            &json!({})
        ));
        assert!(!validate_computer_use_arguments("exec", &json!({})));
        assert!(!validate_computer_use_arguments(
            COMPUTER_CLICK_TOOL,
            &json!({"app_id": "com.apple.Notes", "x": 20})
        ));
        assert!(validate_computer_use_arguments(
            COMPUTER_LAUNCH_APP_TOOL,
            &json!({"app_id": "com.apple.Notes"})
        ));
        assert!(validate_computer_use_arguments(
            COMPUTER_HOVER_TOOL,
            &json!({"app_id": "com.apple.Notes", "x": 1.0, "y": 2.0})
        ));
        assert!(validate_computer_use_arguments(
            COMPUTER_DRAG_TOOL,
            &json!({
                "app_id": "com.apple.Notes",
                "from": {"mark": 1},
                "to": {"x": 10.0, "y": 20.0}
            })
        ));
        assert!(validate_computer_use_arguments(
            COMPUTER_RESIZE_WINDOW_TOOL,
            &json!({"app_id": "com.apple.Notes", "width": 800.0, "height": 600.0})
        ));
    }

    #[test]
    fn tool_name_classifiers_partition_the_surface() {
        for name in COMPUTER_USE_TOOLS {
            assert!(is_computer_use_tool(name), "{name} should be a CU tool");
        }
        for name in COMPUTER_USE_CONTROL_TOOLS {
            assert!(is_computer_use_control_tool(name));
        }
        // Pure reads are not control tools.
        assert!(!is_computer_use_control_tool(COMPUTER_LIST_WINDOWS_TOOL));
        assert!(!is_computer_use_control_tool(COMPUTER_CAPTURE_SCREEN_TOOL));
        assert!(!is_computer_use_control_tool(
            COMPUTER_READ_APP_CONTENT_TOOL
        ));
        assert!(!is_computer_use_control_tool(
            COMPUTER_RETURN_TO_TIDEBREAK_TOOL
        ));
        assert!(!is_computer_use_control_tool(COMPUTER_WAIT_TOOL));
        // Scroll and focus act (they synthesize input and move windows), so
        // they are control tools.
        assert!(is_computer_use_control_tool(COMPUTER_SCROLL_TOOL));
        assert!(is_computer_use_control_tool(COMPUTER_FOCUS_WINDOW_TOOL));
        assert!(is_computer_use_control_tool(COMPUTER_LAUNCH_APP_TOOL));
        assert!(is_computer_use_control_tool(COMPUTER_HOVER_TOOL));
        assert!(is_computer_use_control_tool(COMPUTER_DRAG_TOOL));
        assert!(is_computer_use_control_tool(COMPUTER_RESIZE_WINDOW_TOOL));
        assert!(!is_computer_use_tool("read_file"));
    }

    #[test]
    fn read_app_content_enforces_bounds() {
        assert!(validate_computer_read_app_content_arguments(
            &json!({ "app_id": "com.apple.Notes" })
        ));
        assert!(validate_computer_read_app_content_arguments(
            &json!({ "app_id": "com.apple.Notes", "max_depth": 25, "max_nodes": 2000 })
        ));
        assert!(!validate_computer_read_app_content_arguments(
            &json!({ "app_id": "com.apple.Notes", "max_depth": 26 })
        ));
        assert!(!validate_computer_read_app_content_arguments(
            &json!({ "app_id": "com.apple.Notes", "max_nodes": 2001 })
        ));
        assert!(!validate_computer_read_app_content_arguments(
            &json!({ "app_id": "  " })
        ));
        assert!(!validate_computer_read_app_content_arguments(&json!({})));
    }

    #[test]
    fn type_text_enforces_text_bound_and_target_shape() {
        assert!(validate_computer_type_text_arguments(
            &json!({ "app_id": "com.apple.Notes", "text": "hello" })
        ));
        // Empty text is rejected.
        assert!(!validate_computer_type_text_arguments(
            &json!({ "app_id": "com.apple.Notes", "text": "" })
        ));
        // Over-long text is rejected.
        let long = "x".repeat(MAX_TYPE_TEXT_CHARS + 1);
        assert!(!validate_computer_type_text_arguments(
            &json!({ "app_id": "com.apple.Notes", "text": long })
        ));
        // A mark out of range is rejected.
        assert!(!validate_computer_type_text_arguments(
            &json!({ "app_id": "com.apple.Notes", "text": "hi", "mark": 81 })
        ));
        // A partial coordinate is rejected.
        assert!(!validate_computer_type_text_arguments(
            &json!({ "app_id": "com.apple.Notes", "text": "hi", "x": 10.0 })
        ));
    }

    #[test]
    fn wait_is_bounded() {
        assert!(validate_computer_wait_arguments(&json!({})));
        assert!(validate_computer_wait_arguments(
            &json!({ "seconds": 10.0 })
        ));
        assert!(!validate_computer_wait_arguments(
            &json!({ "seconds": 10.5 })
        ));
        assert!(!validate_computer_wait_arguments(
            &json!({ "seconds": -1.0 })
        ));
        assert!(validate_computer_wait_arguments(
            &json!({ "app_id": "dev.tidebreak.fixture", "condition": { "kind": "app_running" } })
        ));
        assert!(!validate_computer_wait_arguments(
            &json!({ "condition": { "kind": "text_present", "text": "" } })
        ));
        assert!(!validate_computer_wait_arguments(
            &json!({ "condition": { "kind": "text_absent", "text": "x" }, "condition_timeout_seconds": 35.0 })
        ));
        for args in [
            json!({ "condition": { "kind": "app_running" } }),
            json!({ "app_id": "", "condition": { "kind": "window_visible" } }),
            json!({ "app_id": "dev.tidebreak.fixture", "condition": { "kind": "app_running" }, "seconds": 1 }),
            json!({ "app_id": "dev.tidebreak.fixture", "seconds": 1 }),
            json!({ "condition_timeout_seconds": 1 }),
        ] {
            assert!(!validate_computer_wait_arguments(&args), "{args}");
        }
        // serde_json cannot represent a non-finite float, so a NaN/Infinity
        // never survives a wire round-trip; the validator's `is_finite` guard
        // covers the in-memory case.
    }

    #[test]
    fn click_rejects_a_partial_coordinate_and_bad_mark() {
        assert!(validate_computer_click_arguments(
            &json!({ "app_id": "com.apple.Notes", "mark": 3 })
        ));
        assert!(validate_computer_click_arguments(
            &json!({ "app_id": "com.apple.Notes", "x": 100.0, "y": 200.0 })
        ));
        assert!(!validate_computer_click_arguments(
            &json!({ "app_id": "com.apple.Notes", "x": 100.0 })
        ));
        assert!(!validate_computer_click_arguments(
            &json!({ "app_id": "com.apple.Notes", "mark": 0 })
        ));
    }

    #[test]
    fn specs_advertise_no_host_authority_and_deny_unknown_fields() {
        for spec in [
            computer_list_windows_tool_spec(),
            computer_capture_screen_tool_spec(),
            computer_read_app_content_tool_spec(),
            computer_click_tool_spec(),
            computer_type_text_tool_spec(),
            computer_key_press_tool_spec(),
            computer_scroll_tool_spec(),
            computer_focus_window_tool_spec(),
            computer_return_to_tidebreak_tool_spec(),
            computer_wait_tool_spec(),
            computer_launch_app_tool_spec(),
            computer_hover_tool_spec(),
            computer_drag_tool_spec(),
            computer_resize_window_tool_spec(),
        ] {
            assert_eq!(
                spec.input_schema["additionalProperties"], false,
                "{}",
                spec.name
            );
            // The contracts are model proposals; they never carry a grant,
            // token, or absolute host path.
            assert!(!spec.description.contains("grant"), "{}", spec.name);
        }
        assert_eq!(computer_click_tool_spec().name, COMPUTER_CLICK_TOOL);
        assert_eq!(
            computer_launch_app_tool_spec().name,
            COMPUTER_LAUNCH_APP_TOOL
        );
        assert_eq!(
            computer_read_app_content_tool_spec().name,
            COMPUTER_READ_APP_CONTENT_TOOL
        );
    }

    #[test]
    fn execution_mode_defaults_to_background_and_stays_typed() {
        // Absent on the wire means background — the canonical default.
        let args: ComputerClickArgs =
            serde_json::from_value(json!({ "app_id": "com.apple.Notes", "mark": 3 })).unwrap();
        assert_eq!(args.execution_mode, None);
        assert_eq!(
            args.execution_mode.unwrap_or_default(),
            ExecutionMode::Background
        );
        // Foreground round-trips through the snake_case wire value.
        let args: ComputerClickArgs = serde_json::from_value(json!({
            "app_id": "com.apple.Notes",
            "mark": 3,
            "execution_mode": "foreground"
        }))
        .unwrap();
        assert_eq!(args.execution_mode, Some(ExecutionMode::Foreground));
        assert_eq!(
            serde_json::to_value(ExecutionMode::Background).unwrap(),
            json!("background")
        );

        // Every control tool (plus return_to_tidebreak, which moves focus)
        // accepts the typed mode…
        for (name, args) in [
            (COMPUTER_CLICK_TOOL, json!({ "app_id": "a", "mark": 1 })),
            (
                COMPUTER_TYPE_TEXT_TOOL,
                json!({ "app_id": "a", "text": "x" }),
            ),
            (
                COMPUTER_KEY_PRESS_TOOL,
                json!({ "app_id": "a", "key": "tab" }),
            ),
            (COMPUTER_SCROLL_TOOL, json!({ "app_id": "a", "dy": 10.0 })),
            (COMPUTER_FOCUS_WINDOW_TOOL, json!({ "app_id": "a" })),
            (COMPUTER_LAUNCH_APP_TOOL, json!({ "app_id": "a" })),
            (COMPUTER_HOVER_TOOL, json!({ "app_id": "a", "mark": 1 })),
            (
                COMPUTER_DRAG_TOOL,
                json!({ "app_id": "a", "from": { "mark": 1 }, "to": { "mark": 2 } }),
            ),
            (
                COMPUTER_RESIZE_WINDOW_TOOL,
                json!({ "app_id": "a", "width": 800.0, "height": 600.0 }),
            ),
            (COMPUTER_RETURN_TO_TIDEBREAK_TOOL, json!({})),
        ] {
            let mut with_mode = args.clone();
            with_mode["execution_mode"] = json!("background");
            assert!(
                validate_computer_use_arguments(name, &with_mode),
                "{name} accepts background"
            );
            with_mode["execution_mode"] = json!("foreground");
            assert!(
                validate_computer_use_arguments(name, &with_mode),
                "{name} accepts foreground"
            );
            // …and an unknown mode never crosses the trusted-client boundary.
            with_mode["execution_mode"] = json!("takeover");
            assert!(
                !validate_computer_use_arguments(name, &with_mode),
                "{name} rejects an unknown mode"
            );
        }

        // Reads and captures carry no mode: observation never touches focus.
        assert!(!validate_computer_read_app_content_arguments(
            &json!({ "app_id": "a", "execution_mode": "background" })
        ));
        assert!(!validate_computer_capture_screen_arguments(
            &json!({ "execution_mode": "background" })
        ));

        // The schema advertises the field on control tools only.
        assert!(
            computer_click_tool_spec().input_schema["properties"]["execution_mode"].is_object()
        );
        assert!(
            computer_read_app_content_tool_spec().input_schema["properties"]
                .get("execution_mode")
                .is_none()
        );
    }

    #[test]
    fn new_native_primitives_enforce_argument_bounds() {
        assert!(!validate_computer_launch_app_arguments(
            &json!({"app_id": " "})
        ));
        assert!(!validate_computer_launch_app_arguments(&json!({})));
        assert!(validate_computer_hover_arguments(&json!({
            "app_id": "com.apple.Notes",
            "element_id": "0.0",
            "element_fingerprint": "fp"
        })));
        assert!(!validate_computer_hover_arguments(&json!({
            "app_id": "com.apple.Notes",
            "x": 1.0
        })));
        assert!(!validate_computer_drag_arguments(&json!({
            "app_id": "com.apple.Notes",
            "from": {"mark": 1},
            "to": {"x": 1.0}
        })));
        assert!(!validate_computer_drag_arguments(&json!({
            "app_id": "com.apple.Notes",
            "from": {"mark": 1},
            "to": {"x": 1.0, "y": 2.0},
            "duration_ms": 10001
        })));
        assert!(!validate_computer_resize_window_arguments(&json!({
            "app_id": "com.apple.Notes",
            "width": 0.0,
            "height": 600.0
        })));
        assert!(!validate_computer_resize_window_arguments(&json!({
            "app_id": "com.apple.Notes",
            "width": 10001.0,
            "height": 600.0
        })));
        assert!(validate_computer_capture_screen_arguments(&json!({
            "app_id": "com.apple.Notes",
            "window_id": 3,
            "max_dimension": 4096
        })));
        assert!(!validate_computer_capture_screen_arguments(&json!({
            "max_dimension": 4097
        })));
    }
}
