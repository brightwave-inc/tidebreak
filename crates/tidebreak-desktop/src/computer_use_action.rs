//! Activity events for visual agent cursors and the computer-use indicator.
//!
//! Coordinates describe the dispatched action in the named frame. They never
//! describe or change the hardware pointer. Emit only after the runtime knows
//! the target and execution mode; omit geometry when no exact target exists.

use serde::Serialize;
use tauri::{AppHandle, Emitter};

pub(crate) const COMPUTER_USE_ACTION_EVENT: &str = "computer-use-action";

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ComputerUseActionSource {
    Browser,
    Native,
    Chrome,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ComputerUseActionKind {
    Click,
    DoubleClick,
    Type,
    Key,
    Scroll,
    Drag,
    Move,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ComputerUseActionPhase {
    Running,
    Completed,
    Failed,
    ForegroundRequired,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ComputerUseExecutionMode {
    Background,
    Foreground,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ComputerUseCoordinateFrame {
    Viewport,
    Screen,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct ComputerUseActionPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct ComputerUseActionViewport {
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct ComputerUseActionBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// One action's display state. This event grants no control authority.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ComputerUseActionEvent {
    pub action_id: String,
    pub session_id: String,
    pub source: ComputerUseActionSource,
    pub action: ComputerUseActionKind,
    pub phase: ComputerUseActionPhase,
    pub execution_mode: ComputerUseExecutionMode,
    pub coordinate_frame: ComputerUseCoordinateFrame,
    pub started_at_millis: i64,
    pub visible_until_millis: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub point: Option<ComputerUseActionPoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport: Option<ComputerUseActionViewport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_bounds: Option<ComputerUseActionBounds>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_epoch: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_id: Option<String>,
}

pub(crate) fn emit_computer_use_action(app: &AppHandle, event: &ComputerUseActionEvent) {
    crate::native_cursor_overlay::update(app, event);
    if let Err(error) = app.emit(COMPUTER_USE_ACTION_EVENT, event) {
        eprintln!("tidebreak-desktop: could not emit computer-use activity: {error}");
    }
}

/// Build coordinate-free activity until the executor has a verified position.
/// Callers retain this value so completion keeps the same start timestamp.
pub(crate) fn activity_for_call(
    session_id: tidebreak_core::SessionId,
    call: &tidebreak_core::computer_session::ComputerUseCall,
    source: ComputerUseActionSource,
) -> Option<ComputerUseActionEvent> {
    let action_name = if matches!(call.name.as_str(), "chrome_act" | "browser_act") {
        call.arguments.get("action")?.get("type")?.as_str()?
    } else {
        call.name.strip_prefix("computer_")?
    };
    let action = match action_name {
        "click" | "check" | "select" => ComputerUseActionKind::Click,
        "double_click" => ComputerUseActionKind::DoubleClick,
        "type_text" | "type" | "fill" | "clear" => ComputerUseActionKind::Type,
        "key_press" | "press" | "key_chord" => ComputerUseActionKind::Key,
        "scroll" | "scroll_into_view" => ComputerUseActionKind::Scroll,
        "drag" => ComputerUseActionKind::Drag,
        "hover" | "focus_window" | "resize_window" | "launch_app" | "return_to_tidebreak" => {
            ComputerUseActionKind::Move
        }
        _ => return None,
    };
    let now = action_time_millis();
    Some(ComputerUseActionEvent {
        action_id: call.request_id.to_string(),
        session_id: session_id.to_string(),
        source,
        action,
        phase: ComputerUseActionPhase::Running,
        execution_mode: if call
            .arguments
            .get("execution_mode")
            .and_then(serde_json::Value::as_str)
            == Some("foreground")
        {
            ComputerUseExecutionMode::Foreground
        } else {
            ComputerUseExecutionMode::Background
        },
        coordinate_frame: if matches!(source, ComputerUseActionSource::Native) {
            ComputerUseCoordinateFrame::Screen
        } else {
            ComputerUseCoordinateFrame::Viewport
        },
        started_at_millis: now,
        visible_until_millis: now + 30_000,
        point: None,
        viewport: None,
        target_bounds: None,
        browser_id: None,
        workspace_id: None,
        instance_id: None,
        document_epoch: call
            .arguments
            .get("documentEpoch")
            .and_then(serde_json::Value::as_u64),
        bundle_id: call
            .arguments
            .get("app_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        window_id: call
            .arguments
            .get("window_id")
            .and_then(serde_json::Value::as_u64)
            .and_then(|id| u32::try_from(id).ok()),
        capture_id: None,
    })
}

/// Track a request until it settles. A dropped dispatch emits cancellation so
/// a stopped or abandoned request cannot leave the activity indicator running.
/// Running means a request is in progress; it does not claim input was sent.
pub(crate) struct CallActivity {
    event: Option<ComputerUseActionEvent>,
    emit: Box<dyn Fn(&ComputerUseActionEvent) + Send + Sync>,
}

impl CallActivity {
    pub(crate) fn start(app: &AppHandle, event: ComputerUseActionEvent) -> Self {
        let app = app.clone();
        Self::with_emitter(event, move |event| emit_computer_use_action(&app, event))
    }

    fn with_emitter(
        mut event: ComputerUseActionEvent,
        emit: impl Fn(&ComputerUseActionEvent) + Send + Sync + 'static,
    ) -> Self {
        event.started_at_millis = action_time_millis();
        event.visible_until_millis = event.started_at_millis + 30_000;
        emit(&event);
        Self {
            event: Some(event),
            emit: Box::new(emit),
        }
    }

    /// Geometry comes only from the helper's completed action result.
    pub(crate) fn set_native_cursor(&mut self, cursor: Option<&serde_json::Value>) {
        let (Some(event), Some(cursor)) = (self.event.as_mut(), cursor) else {
            return;
        };
        if !matches!(event.source, ComputerUseActionSource::Native) {
            return;
        }
        let number = |value: &serde_json::Value, field: &str| {
            value.get(field)?.as_f64().filter(|value| value.is_finite())
        };
        let Some(window_id) = cursor
            .get("window_id")
            .and_then(serde_json::Value::as_u64)
            .and_then(|id| u32::try_from(id).ok())
            .filter(|id| *id != 0)
        else {
            return;
        };
        let (Some(point), Some(bounds)) = (cursor.get("point"), cursor.get("window_bounds")) else {
            return;
        };
        let (Some(x), Some(y), Some(left), Some(top), Some(width), Some(height)) = (
            number(point, "x"),
            number(point, "y"),
            number(bounds, "x"),
            number(bounds, "y"),
            number(bounds, "width"),
            number(bounds, "height"),
        ) else {
            return;
        };
        if width <= 0.0
            || height <= 0.0
            || x < left
            || y < top
            || x > left + width
            || y > top + height
        {
            return;
        }
        event.point = Some(ComputerUseActionPoint { x, y });
        event.target_bounds = Some(ComputerUseActionBounds {
            x: left,
            y: top,
            width,
            height,
        });
        event.window_id = Some(window_id);
    }

    pub(crate) fn finish(mut self, success: bool, error_code: Option<&str>) {
        let phase = match error_code {
            Some("requires_foreground" | "foreground_required") => {
                ComputerUseActionPhase::ForegroundRequired
            }
            Some(
                "stopped_by_user"
                | "stopped"
                | "interrupted"
                | "computer_use_cancelled"
                | "cancelled",
            ) => ComputerUseActionPhase::Cancelled,
            _ if success => ComputerUseActionPhase::Completed,
            _ => ComputerUseActionPhase::Failed,
        };
        self.settle(phase);
    }

    fn settle(&mut self, phase: ComputerUseActionPhase) {
        if let Some(mut event) = self.event.take() {
            event.phase = phase;
            event.visible_until_millis = action_time_millis() + 1_500;
            (self.emit)(&event);
        }
    }
}

impl Drop for CallActivity {
    fn drop(&mut self) {
        self.settle(ComputerUseActionPhase::Cancelled);
    }
}

fn action_time_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_requests_cancel_and_completed_requests_emit_only_once() {
        use std::sync::{Arc, Mutex};
        let call = tidebreak_core::computer_session::ComputerUseCall {
            request_id: uuid::Uuid::new_v4(),
            name: "computer_click".into(),
            arguments: serde_json::json!({"app_id": "test.fixture"}),
        };
        let event = activity_for_call(
            tidebreak_core::SessionId::new(),
            &call,
            ComputerUseActionSource::Native,
        )
        .unwrap();
        let phases = Arc::new(Mutex::new(Vec::new()));
        let capture = phases.clone();
        let activity = CallActivity::with_emitter(event.clone(), move |event| {
            capture
                .lock()
                .unwrap()
                .push(serde_json::to_value(event.phase).unwrap());
        });
        drop(activity);
        assert_eq!(*phases.lock().unwrap(), vec!["running", "cancelled"]);
        phases.lock().unwrap().clear();
        let capture = phases.clone();
        CallActivity::with_emitter(event, move |event| {
            capture
                .lock()
                .unwrap()
                .push(serde_json::to_value(event.phase).unwrap());
        })
        .finish(true, None);
        assert_eq!(*phases.lock().unwrap(), vec!["running", "completed"]);
    }

    #[test]
    fn activity_wire_names_match_the_shared_ui_contract() {
        let event = ComputerUseActionEvent {
            action_id: "action-1".into(),
            session_id: "session-1".into(),
            source: ComputerUseActionSource::Native,
            action: ComputerUseActionKind::Type,
            phase: ComputerUseActionPhase::ForegroundRequired,
            execution_mode: ComputerUseExecutionMode::Foreground,
            coordinate_frame: ComputerUseCoordinateFrame::Screen,
            started_at_millis: 1_000,
            visible_until_millis: 3_000,
            point: None,
            viewport: None,
            target_bounds: None,
            browser_id: None,
            workspace_id: None,
            instance_id: None,
            document_epoch: None,
            bundle_id: Some("test.fixture".into()),
            window_id: Some(42),
            capture_id: None,
        };
        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["phase"], "foreground_required");
        assert_eq!(value["coordinateFrame"], "screen");
        assert_eq!(value["executionMode"], "foreground");
        assert_eq!(value["windowId"], 42);
        assert!(value.get("point").is_none());
        assert!(value.get("captureId").is_none());
    }
}
