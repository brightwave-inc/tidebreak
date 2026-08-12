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
/// Return focus to the OpenWave window (e.g. to read the transcript).
pub const COMPUTER_RETURN_TO_OPENWAVE_TOOL: &str = "computer_return_to_openwave";
/// Wait a bounded number of seconds (e.g. for an app to finish an action).
pub const COMPUTER_WAIT_TOOL: &str = "computer_wait";

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

/// All ten computer-use tool names.
pub const COMPUTER_USE_TOOLS: [&str; 10] = [
    COMPUTER_LIST_WINDOWS_TOOL,
    COMPUTER_CAPTURE_SCREEN_TOOL,
    COMPUTER_READ_APP_CONTENT_TOOL,
    COMPUTER_CLICK_TOOL,
    COMPUTER_TYPE_TEXT_TOOL,
    COMPUTER_KEY_PRESS_TOOL,
    COMPUTER_SCROLL_TOOL,
    COMPUTER_FOCUS_WINDOW_TOOL,
    COMPUTER_RETURN_TO_OPENWAVE_TOOL,
    COMPUTER_WAIT_TOOL,
];

/// The control (acting) tools, which require the `ControlApp` grant and gate
/// behind the `ComputerMayControlApp` approval kind. Reads never card per-call
/// once their grant exists.
pub const COMPUTER_USE_CONTROL_TOOLS: [&str; 3] = [
    COMPUTER_CLICK_TOOL,
    COMPUTER_TYPE_TEXT_TOOL,
    COMPUTER_KEY_PRESS_TOOL,
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
/// for seeing first and acting second, acting is slower and may need approval,
/// and the user can stop it at any time.
const ACTING_NOTE: &str = "\n\nActing is slower than reading and may need the user's approval; prefer reading first and act only when it moves the task. The user can stop control at any time.";

/// Shared targeting guidance: prefer a Set-of-Marks number or an element
/// identity over raw coordinates.
const TARGETING_NOTE: &str = "Target by `mark` (a number from the last annotated screenshot) or by `element_id` + `element_fingerprint` from `computer_read_app_content`. Use `x`/`y` coordinates only when the app exposes no usable accessibility element.";

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
}

fn default_true() -> bool {
    true
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
    #[schemars(
        with = "ClickButton",
        description = "Mouse button (default left)."
    )]
    pub button: Option<ClickButton>,
    /// Double-click when true. Defaults to a single click.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Double-click when true (default single).")]
    pub double: Option<bool>,
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
}

/// Canonical arguments for [`COMPUTER_KEY_PRESS_TOOL`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerKeyPressArgs {
    /// The app to send the key to, by bundle id.
    #[schemars(description = "App bundle id.")]
    pub app_id: String,
    /// The key name (e.g. "return", "tab", "escape", "a", "left").
    #[schemars(length(min = 1), description = "Key name (e.g. \"return\", \"tab\", \"a\").")]
    pub key: String,
    /// Chord modifiers to hold while pressing the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Chord modifiers to hold (cmd/shift/ctrl/alt/fn).")]
    pub modifiers: Option<Vec<KeyModifier>>,
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
}

/// Canonical arguments for [`COMPUTER_RETURN_TO_OPENWAVE_TOOL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerReturnToOpenwaveArgs {}

/// Canonical arguments for [`COMPUTER_WAIT_TOOL`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerWaitArgs {
    /// How long to wait, in seconds (default 1, max 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Seconds to wait (default 1, max 10).")]
    pub seconds: Option<f64>,
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

validate_fn!(validate_computer_list_windows_arguments, ComputerListWindowsArgs);
validate_fn!(validate_computer_focus_window_arguments, ComputerFocusWindowArgs);
validate_fn!(
    validate_computer_return_to_openwave_arguments,
    ComputerReturnToOpenwaveArgs
);

/// Validate a `computer_capture_screen` payload.
#[must_use]
pub fn validate_computer_capture_screen_arguments(arguments: &Value) -> bool {
    parse::<ComputerCaptureScreenArgs>(arguments).is_some()
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
    args.max_depth.is_none_or(|d| d >= 1 && d <= MAX_READ_DEPTH)
        && args.max_nodes.is_none_or(|n| n >= 1 && n <= MAX_READ_NODES)
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
    args.seconds
        .is_none_or(|s| s.is_finite() && s >= 0.0 && s <= MAX_WAIT_SECONDS)
}

// MARK: - Tool specs

/// Tool contract for [`COMPUTER_LIST_WINDOWS_TOOL`].
#[must_use]
pub fn computer_list_windows_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerListWindowsArgs>(
        COMPUTER_LIST_WINDOWS_TOOL,
        "List the on-screen windows, optionally for one app. Use this first to discover what is open and to find an app's bundle id before capturing or reading it.",
    )
}

/// Tool contract for [`COMPUTER_CAPTURE_SCREEN_TOOL`].
#[must_use]
pub fn computer_capture_screen_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerCaptureScreenArgs>(
        COMPUTER_CAPTURE_SCREEN_TOOL,
        "Capture the screen as an image — the whole display, or one app's windows. Annotates interactive elements with numbered marks so you can act on them by number. Use this to see what is on screen before acting.",
    )
}

/// Tool contract for [`COMPUTER_READ_APP_CONTENT_TOOL`].
#[must_use]
pub fn computer_read_app_content_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerReadAppContentArgs>(
        COMPUTER_READ_APP_CONTENT_TOOL,
        "Read an app's accessibility tree: on-screen text, element roles, values, and bounds. This is the primary way to see an app — cheaper and more reliable than a screenshot, and it yields the element ids and fingerprints the acting tools target.",
    )
}

/// Tool contract for [`COMPUTER_CLICK_TOOL`].
#[must_use]
pub fn computer_click_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerClickArgs>(
        COMPUTER_CLICK_TOOL,
        &format!(
            "Click an element or point in an app. {TARGETING_NOTE}{ACTING_NOTE}"
        ),
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
            "Press a key, optionally with chord modifiers, in the focused app. Use for keyboard shortcuts and navigation keys.{ACTING_NOTE}"
        ),
    )
}

/// Tool contract for [`COMPUTER_SCROLL_TOOL`].
#[must_use]
pub fn computer_scroll_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerScrollArgs>(
        COMPUTER_SCROLL_TOOL,
        &format!(
            "Scroll an element or point by a pixel delta. {TARGETING_NOTE}"
        ),
    )
}

/// Tool contract for [`COMPUTER_FOCUS_WINDOW_TOOL`].
#[must_use]
pub fn computer_focus_window_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerFocusWindowArgs>(
        COMPUTER_FOCUS_WINDOW_TOOL,
        "Bring an app (or one of its windows) to the front. Use this to return to an app after focus shifted away.",
    )
}

/// Tool contract for [`COMPUTER_RETURN_TO_OPENWAVE_TOOL`].
#[must_use]
pub fn computer_return_to_openwave_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerReturnToOpenwaveArgs>(
        COMPUTER_RETURN_TO_OPENWAVE_TOOL,
        "Return focus to the OpenWave window. Use after acting in another app when you need to read the transcript or report back.",
    )
}

/// Tool contract for [`COMPUTER_WAIT_TOOL`].
#[must_use]
pub fn computer_wait_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ComputerWaitArgs>(
        COMPUTER_WAIT_TOOL,
        "Wait a bounded number of seconds, e.g. for an app to finish an action or a window to appear, before the next read or capture.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_name_classifiers_partition_the_surface() {
        for name in COMPUTER_USE_TOOLS {
            assert!(is_computer_use_tool(name), "{name} should be a CU tool");
        }
        for name in COMPUTER_USE_CONTROL_TOOLS {
            assert!(is_computer_use_control_tool(name));
        }
        // Reads are not control tools.
        assert!(!is_computer_use_control_tool(COMPUTER_LIST_WINDOWS_TOOL));
        assert!(!is_computer_use_control_tool(COMPUTER_CAPTURE_SCREEN_TOOL));
        assert!(!is_computer_use_control_tool(COMPUTER_READ_APP_CONTENT_TOOL));
        assert!(!is_computer_use_control_tool(COMPUTER_SCROLL_TOOL));
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
        assert!(!validate_computer_read_app_content_arguments(&json!({ "app_id": "  " })));
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
        assert!(validate_computer_wait_arguments(&json!({ "seconds": 10.0 })));
        assert!(!validate_computer_wait_arguments(&json!({ "seconds": 10.5 })));
        assert!(!validate_computer_wait_arguments(&json!({ "seconds": -1.0 })));
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
            computer_return_to_openwave_tool_spec(),
            computer_wait_tool_spec(),
        ] {
            assert_eq!(spec.input_schema["additionalProperties"], false, "{}", spec.name);
            // The contracts are model proposals; they never carry a grant,
            // token, or absolute host path.
            assert!(!spec.description.contains("grant"), "{}", spec.name);
        }
        assert_eq!(computer_click_tool_spec().name, COMPUTER_CLICK_TOOL);
        assert_eq!(
            computer_read_app_content_tool_spec().name,
            COMPUTER_READ_APP_CONTENT_TOOL
        );
    }
}
