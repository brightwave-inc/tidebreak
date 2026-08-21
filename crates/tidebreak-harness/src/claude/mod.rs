//! Claude Code adapter. Reference tier.

pub mod approvals;
pub mod browser;
pub mod parse;
pub mod session;

use async_trait::async_trait;
use tidebreak_core::{CapLevel, HarnessCaps, HarnessKind};

use crate::claude::session::ClaudeSession;
use crate::probe::{observe_version, probe_shell, HostEnv};
use crate::{HarnessAdapter, HarnessError, HarnessProbe, HarnessSession, SessionSpec};

/// Claude Code adapter. Capabilities below are for the captured version
/// 2.1.233: verified flags are `Supported`/`Unsupported`; anything not
/// seen in a fixture is `Unknown`.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeCodeAdapter;

impl ClaudeCodeAdapter {
    /// Construct the adapter.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

fn claude_settings_models() -> Vec<crate::ListedHarnessModel> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let Some(home) = home else {
        return Vec::new();
    };
    let path = home.join(".claude").join("settings.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let current = value
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty());
    let mut models = Vec::new();
    if let Some(list) = value
        .get("availableModels")
        .and_then(serde_json::Value::as_array)
    {
        for item in list {
            let Some(id) = item.as_str().map(str::trim).filter(|id| !id.is_empty()) else {
                continue;
            };
            models.push(crate::ListedHarnessModel {
                label: crate::display_model_label(id),
                default: current == Some(id),
                id: id.to_owned(),
            });
        }
    }
    if let Some(current) = current {
        if !models.iter().any(|model| model.id == current) {
            models.insert(
                0,
                crate::ListedHarnessModel {
                    label: crate::display_model_label(current),
                    id: current.to_owned(),
                    default: true,
                },
            );
        }
    }
    models
}

#[async_trait]
impl HarnessAdapter for ClaudeCodeAdapter {
    fn kind(&self) -> HarnessKind {
        HarnessKind::ClaudeCode
    }

    async fn probe(&self, host: &HostEnv) -> HarnessProbe {
        match probe_shell(host, "claude").await {
            Ok(capture) => {
                let version = observe_version(&capture.binary, &capture.env).await.ok();
                // Auth observation is not captured for 2.1.233. Do not guess.
                HarnessProbe {
                    found: true,
                    binary_path: Some(capture.binary),
                    version,
                    authenticated: None,
                    stderr: capture.stderr,
                    env: capture.env,
                    commands: Vec::new(),
                }
            }
            Err(err) => HarnessProbe {
                found: false,
                binary_path: None,
                version: None,
                authenticated: None,
                stderr: err.to_string(),
                env: Vec::new(),
                commands: Vec::new(),
            },
        }
    }

    fn capabilities(&self, _probe: &HarnessProbe) -> HarnessCaps {
        // Honest values for v2.1.233, from captured fixtures and `--help`.
        HarnessCaps {
            resume: CapLevel::Supported,
            streaming_deltas: CapLevel::Supported,
            // Permission flags are documented on the 2.1 line. The product
            // pins 2.1.234 and offers every mode that pin can honor.
            structured_approvals: CapLevel::Supported,
            mid_turn_steering: CapLevel::Unknown,
            plan_mode: CapLevel::Supported,
            auto_mode: CapLevel::Supported,
            allow_mode: CapLevel::Supported,
            reasoning_levels: CapLevel::Unknown,
            native_file_change_events: CapLevel::Unknown,
            native_interrupt: CapLevel::Supported,
            image_input: CapLevel::Supported,
            slash_commands: CapLevel::Unknown,
        }
    }

    async fn list_models(&self, probe: &HarnessProbe) -> Vec<crate::ListedHarnessModel> {
        // `claude models` is not a catalog command — it starts a session
        // with that prompt. Read the user's Claude settings instead.
        let _ = probe;
        claude_settings_models()
    }

    async fn launch(&self, spec: SessionSpec) -> Result<Box<dyn HarnessSession>, HarnessError> {
        if !spec.binary.is_absolute() {
            return Err(HarnessError::NotFound);
        }
        Ok(Box::new(ClaudeSession::new(spec)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::parse::ClaudeStreamParser;
    use crate::HarnessEvent;
    use std::path::PathBuf;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/claude-code/2.1.233")
    }

    fn replay(name: &str) -> (Vec<HarnessEvent>, u64) {
        let path = fixture_dir().join(format!("{name}.ndjson"));
        let input = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("missing fixture {}: {err}", path.display()));
        let out = ClaudeStreamParser::parse_ndjson(&input);
        let expected_path = fixture_dir().join(format!("{name}.expected.json"));
        if std::env::var_os("UPDATE_HARNESS_FIXTURES").is_some() {
            let rendered = format!("{}\n", serde_json::to_string_pretty(&out.events).unwrap());
            std::fs::write(&expected_path, rendered).unwrap();
        } else {
            let expected = std::fs::read_to_string(&expected_path).unwrap_or_else(|err| {
                panic!(
                    "missing expected sequence {}: {err}; regenerate with \
                     UPDATE_HARNESS_FIXTURES=1 cargo test -p tidebreak-harness",
                    expected_path.display()
                )
            });
            let actual = format!("{}\n", serde_json::to_string_pretty(&out.events).unwrap());
            assert_eq!(
                expected.replace("\r\n", "\n"),
                actual,
                "normalized sequence for {name} drifted from the fixture"
            );
        }
        (out.events, out.unrecognized)
    }

    #[test]
    fn fixture_replay_plain_text() {
        let (events, unrecognized) = replay("plain-text");
        assert_eq!(
            unrecognized, 0,
            "captured hook/status metadata is recognized protocol noise"
        );
        assert!(events
            .iter()
            .any(|event| matches!(event, HarnessEvent::SessionStarted { .. })));
        assert!(events
            .iter()
            .any(|event| matches!(event, HarnessEvent::AssistantDelta { .. })));
        assert!(events
            .iter()
            .any(|event| matches!(event, HarnessEvent::TurnCompleted { .. })));
    }

    #[test]
    fn fixture_replay_tool_use() {
        let (events, _) = replay("tool-use");
        // `content_block_start` opens the call with `input: {}`, so the
        // started detail names nothing. The complete arguments arrive on the
        // `assistant` message and ride the completion as a correction —
        // without it the transcript line falls back to "Read".
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolStarted { name, detail, .. }
                if name == "Read" && detail.specificity() == 0
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolCompleted {
                detail: Some(tidebreak_core::ToolDetail::FileRead { path }),
                ..
            } if path == "/workspace/README.md"
        )));
    }

    /// The ring's numerator is one prompt, not the turn's summed spend.
    ///
    /// `tool-use` ran three model calls: the four spend counts total 148,401
    /// prompt-side tokens while the last call's own prompt was 49,603. On a
    /// 200k window that is the difference between reading 74% and 25%.
    #[test]
    fn context_tokens_are_the_last_call_not_the_turn_total() {
        let (events, _) = replay("tool-use");
        let usage = completed_usage(&events);
        assert_eq!(usage.context_tokens, 49_603);
        let spend =
            usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens;
        assert_eq!(spend, 148_401);
        assert!(
            usage.context_tokens < spend,
            "summed spend must not be mistaken for occupancy"
        );
    }

    /// A single-call turn publishes no `iterations`, and the top-level object
    /// is that one call — so occupancy and prompt-side spend agree.
    #[test]
    fn a_result_without_iterations_reads_its_own_prompt() {
        let (events, _) = replay("subagent-task");
        let usage = completed_usage(&events);
        assert_eq!(usage.context_tokens, 8);
        assert_eq!(usage.input_tokens, 8);
    }

    fn completed_usage(events: &[HarnessEvent]) -> tidebreak_core::CodeUsage {
        events
            .iter()
            .find_map(|event| match event {
                HarnessEvent::TurnCompleted { usage } => Some(usage.clone()),
                _ => None,
            })
            .expect("the fixture completes its turn")
    }

    #[test]
    fn fixture_replay_permission_denied() {
        let (events, _) = replay("permission-denied");
        assert!(events
            .iter()
            .any(|event| matches!(event, HarnessEvent::HarnessNotice { .. })));
        assert!(events.iter().all(|event| {
            !matches!(
                event,
                HarnessEvent::ApprovalRequested { .. } | HarnessEvent::ApprovalResolved { .. }
            )
        }));
    }

    #[test]
    fn fixture_replay_resume() {
        let (events, _) = replay("resume");
        let started = events
            .iter()
            .find_map(|event| match event {
                HarnessEvent::SessionStarted { resume_ref, .. } => resume_ref.clone(),
                _ => None,
            })
            .expect("resume fixture must report a session id");
        assert!(!started.is_empty());
    }

    #[test]
    fn fixture_replay_interrupt() {
        let (events, _) = replay("interrupt");
        assert!(events
            .iter()
            .any(|event| matches!(event, HarnessEvent::TurnInterrupted)));
    }

    #[test]
    fn fixture_replay_approval_allow() {
        let (events, _) = replay("approval-allow");
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolStarted { name, .. } if name == "Write"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolCompleted { outcome, .. }
                if *outcome == tidebreak_core::ToolOutcome::Succeeded
        )));
        assert!(events
            .iter()
            .any(|event| matches!(event, HarnessEvent::TurnCompleted { .. })));
        assert!(events
            .iter()
            .all(|event| { !matches!(event, HarnessEvent::ApprovalRequested { .. }) }));
    }

    #[test]
    fn fixture_replay_approval_deny_with_feedback() {
        let (events, _) = replay("approval-deny-with-feedback");
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolCompleted { outcome, preview, .. }
                if *outcome == tidebreak_core::ToolOutcome::Failed
                    && preview.contains("fixtures directory")
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::AssistantMessage { text, .. } if text.contains("DENIED")
        )));
    }

    #[test]
    fn fixture_replay_approval_request_parses_the_mcp_payload() {
        let path = fixture_dir().join("approval-request.mcp.json");
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let request: crate::claude::approvals::PermissionPromptRequest =
            serde_json::from_value(raw["params"]["arguments"].clone()).unwrap();
        let event = crate::claude::approvals::event_from_prompt_request(&request);
        match event {
            HarnessEvent::ApprovalRequested { harness_ref, raw } => {
                assert_eq!(harness_ref.call_id, request.tool_use_id);
                assert_eq!(raw["tool_name"], "Write");
            }
            other => panic!("{other:?}"),
        }
        let (events, _) = replay("approval-request");
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolStarted { name, .. } if name == "Write"
        )));
    }

    #[test]
    fn fixture_replay_subagent_task() {
        // Synthesized fixture: a `Task` call spans a subagent (decision 52).
        // Nested lines carry the spanning call's id in `parent_tool_use_id`;
        // the parent's own lines say null.
        let (events, unrecognized) = replay("subagent-task");
        assert_eq!(unrecognized, 0);
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolStarted { call_id, name, detail, parent_call_id: None }
                if call_id == "toolu_01TaskSpan"
                    && name == "Task"
                    && detail.subject() == "Find the config parser (general-purpose)"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolStarted { name, parent_call_id: Some(parent), .. }
                if name == "Read" && parent == "toolu_01TaskSpan"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolCompleted { call_id, parent_call_id: Some(parent), .. }
                if call_id == "toolu_01ChildRead" && parent == "toolu_01TaskSpan"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::AssistantMessage { parent_call_id: Some(parent), .. }
                if parent == "toolu_01TaskSpan"
        )));
        // The Task's own result closes the span with no parent of its own.
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolCompleted {
                call_id,
                outcome: tidebreak_core::ToolOutcome::Succeeded,
                parent_call_id: None,
                ..
            } if call_id == "toolu_01TaskSpan"
        )));
    }

    #[test]
    fn adapter_has_a_fixtures_directory_with_a_manifest() {
        assert!(fixture_dir().join("manifest.toml").is_file());
    }

    #[test]
    fn capabilities_for_2_1_233_are_honest() {
        let caps = ClaudeCodeAdapter::new().capabilities(&HarnessProbe {
            found: true,
            binary_path: None,
            version: Some("2.1.233 (Claude Code)".into()),
            authenticated: None,
            stderr: String::new(),
            env: Vec::new(),
            commands: Vec::new(),
        });
        assert_eq!(caps.resume, CapLevel::Supported);
        assert_eq!(caps.streaming_deltas, CapLevel::Supported);
        assert_eq!(caps.native_interrupt, CapLevel::Supported);
        assert_eq!(caps.plan_mode, CapLevel::Supported);
        assert_eq!(caps.auto_mode, CapLevel::Supported);
        assert_eq!(caps.allow_mode, CapLevel::Supported);
        assert_eq!(caps.structured_approvals, CapLevel::Supported);
        assert_eq!(caps.mid_turn_steering, CapLevel::Unknown);
        assert_eq!(caps.reasoning_levels, CapLevel::Unknown);
        assert_eq!(caps.native_file_change_events, CapLevel::Unknown);
        assert_eq!(caps.image_input, CapLevel::Supported);
        assert_eq!(caps.slash_commands, CapLevel::Unknown);
    }

    #[test]
    fn permission_modes_stay_supported_off_the_original_capture() {
        let caps = ClaudeCodeAdapter::new().capabilities(&HarnessProbe {
            found: true,
            binary_path: None,
            version: Some("2.1.300 (Claude Code)".into()),
            authenticated: None,
            stderr: String::new(),
            env: Vec::new(),
            commands: Vec::new(),
        });
        assert_eq!(caps.structured_approvals, CapLevel::Supported);
        assert_eq!(caps.allow_mode, CapLevel::Supported);
    }

    #[test]
    fn permission_mode_mapping_is_explicit() {
        use crate::claude::session::permission_mode_flags;
        use tidebreak_core::CodePermissionMode;
        assert_eq!(
            permission_mode_flags(CodePermissionMode::Plan),
            ["--permission-mode", "plan"]
        );
        assert_eq!(
            permission_mode_flags(CodePermissionMode::Ask),
            ["--permission-mode", "manual"]
        );
        assert_eq!(
            permission_mode_flags(CodePermissionMode::Auto),
            ["--permission-mode", "acceptEdits"]
        );
        assert_eq!(
            permission_mode_flags(CodePermissionMode::Allow),
            [
                "--dangerously-skip-permissions",
                "--allow-dangerously-skip-permissions"
            ]
        );
    }
}
