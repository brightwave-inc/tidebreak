//! Claude Code adapter. Reference tier.

pub mod approvals;
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

#[async_trait]
impl HarnessAdapter for ClaudeCodeAdapter {
    fn kind(&self) -> HarnessKind {
        HarnessKind::ClaudeCode
    }

    async fn probe(&self, host: &HostEnv) -> HarnessProbe {
        match probe_shell(host, "claude").await {
            Ok(capture) => {
                let version = observe_version(&capture.binary).await.ok();
                // Auth observation is not captured for 2.1.233. Do not guess.
                HarnessProbe {
                    found: true,
                    binary_path: Some(capture.binary),
                    version,
                    authenticated: None,
                    stderr: capture.stderr,
                    env: capture.env,
                }
            }
            Err(err) => HarnessProbe {
                found: false,
                binary_path: None,
                version: None,
                authenticated: None,
                stderr: err.to_string(),
                env: Vec::new(),
            },
        }
    }

    fn capabilities(&self, _probe: &HarnessProbe) -> HarnessCaps {
        // Honest values for v2.1.233, from captured fixtures and `--help`.
        HarnessCaps {
            resume: CapLevel::Supported,
            streaming_deltas: CapLevel::Supported,
            // Hidden `--permission-prompt-tool` + HTTP MCP captured on 2.1.233.
            structured_approvals: if version_is_2_1_233(_probe.version.as_deref()) {
                CapLevel::Supported
            } else {
                CapLevel::Unknown
            },
            mid_turn_steering: CapLevel::Unknown,
            plan_mode: CapLevel::Supported,
            // `--permission-mode acceptEdits`; supervised by the approval
            // channel, so it carries the same version gate — unsupervised
            // Auto has not been probed on other versions.
            auto_mode: if version_is_2_1_233(_probe.version.as_deref()) {
                CapLevel::Supported
            } else {
                CapLevel::Unknown
            },
            reasoning_levels: CapLevel::Unknown,
            native_file_change_events: CapLevel::Unknown,
            native_interrupt: CapLevel::Supported,
        }
    }

    async fn launch(&self, spec: SessionSpec) -> Result<Box<dyn HarnessSession>, HarnessError> {
        if !spec.binary.is_absolute() {
            return Err(HarnessError::NotFound);
        }
        Ok(Box::new(ClaudeSession::new(spec)))
    }
}

fn version_is_2_1_233(version: Option<&str>) -> bool {
    version.is_some_and(|value| value.starts_with("2.1.233"))
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
                expected, actual,
                "normalized sequence for {name} drifted from the fixture"
            );
        }
        (out.events, out.unrecognized)
    }

    #[test]
    fn fixture_replay_plain_text() {
        let (events, unrecognized) = replay("plain-text");
        assert!(unrecognized > 0, "hook/status events must be counted");
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
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolStarted { name, .. } if name == "Read"
        )));
        assert!(events
            .iter()
            .any(|event| matches!(event, HarnessEvent::ToolCompleted { .. })));
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
            HarnessEvent::AssistantMessage { text } if text.contains("DENIED")
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
        });
        assert_eq!(caps.resume, CapLevel::Supported);
        assert_eq!(caps.streaming_deltas, CapLevel::Supported);
        assert_eq!(caps.native_interrupt, CapLevel::Supported);
        assert_eq!(caps.plan_mode, CapLevel::Supported);
        assert_eq!(caps.structured_approvals, CapLevel::Supported);
        assert_eq!(caps.mid_turn_steering, CapLevel::Unknown);
        assert_eq!(caps.reasoning_levels, CapLevel::Unknown);
        assert_eq!(caps.native_file_change_events, CapLevel::Unknown);
    }

    #[test]
    fn structured_approvals_stay_unknown_off_the_captured_version() {
        let caps = ClaudeCodeAdapter::new().capabilities(&HarnessProbe {
            found: true,
            binary_path: None,
            version: Some("2.1.300 (Claude Code)".into()),
            authenticated: None,
            stderr: String::new(),
            env: Vec::new(),
        });
        assert_eq!(caps.structured_approvals, CapLevel::Unknown);
    }
}
