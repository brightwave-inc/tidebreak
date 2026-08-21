//! opencode adapter. Tertiary tier.
//!
//! Process model for 1.18.18: one long-lived `opencode serve` child per
//! session, driven over HTTP + a directory-scoped SSE event stream. Chosen
//! over `opencode run --format json` because the installed version's serve
//! API is real — sessions, messages, `/event`, and a permission reply
//! surface — and the prompt stays off argv.

pub mod parse;
pub mod session;

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tidebreak_core::{CapLevel, HarnessCaps, HarnessKind};
use tokio::process::Command;
use tokio::time::timeout;

use crate::opencode::session::attach;
use crate::probe::{observe_version, probe_shell, HostEnv};
use crate::{HarnessAdapter, HarnessError, HarnessProbe, HarnessSession, SessionSpec};

const AUTH_TIMEOUT: Duration = Duration::from_secs(15);

/// opencode adapter. Capabilities below are for the captured version
/// 1.18.18: verified flags are `Supported`/`Unsupported`; anything not
/// seen in a fixture is `Unknown`.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpencodeAdapter;

impl OpencodeAdapter {
    /// Construct the adapter.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl HarnessAdapter for OpencodeAdapter {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Opencode
    }

    async fn probe(&self, host: &HostEnv) -> HarnessProbe {
        match probe_shell(host, "opencode").await {
            Ok(capture) => {
                let version = observe_version(&capture.binary, &capture.env).await.ok();
                let authenticated = observe_auth(&capture.binary, &capture.env).await;
                HarnessProbe {
                    found: true,
                    binary_path: Some(capture.binary),
                    version,
                    authenticated,
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
        HarnessCaps {
            resume: CapLevel::Supported,
            streaming_deltas: CapLevel::Supported,
            structured_approvals: CapLevel::Supported,
            // v2 POST /api/session/{id}/prompt delivery=steer exists in
            // OpenAPI; not captured.
            mid_turn_steering: CapLevel::Unknown,
            plan_mode: CapLevel::Supported,
            // Workspace-write ruleset with sensitive actions still asking
            // over the permission API; supervised Auto.
            auto_mode: CapLevel::Supported,
            // build agent with every permission rule allow; not `--auto`.
            allow_mode: CapLevel::Supported,
            // 1.18.18 `--help` lists no effort or reasoning flag, and the
            // server's session and prompt bodies carry no field for one.
            reasoning_levels: CapLevel::Unsupported,
            // session.diff was empty arrays; file.edited was not seen.
            native_file_change_events: CapLevel::Unknown,
            native_interrupt: CapLevel::Supported,
            image_input: CapLevel::Unknown,
            slash_commands: CapLevel::Unknown,
        }
    }

    async fn launch(&self, spec: SessionSpec) -> Result<Box<dyn HarnessSession>, HarnessError> {
        if !spec.binary.is_absolute() {
            return Err(HarnessError::NotFound);
        }
        Ok(Box::new(attach(spec).await?))
    }
}

/// `opencode auth list` — "N credentials" vs "0 credentials". Never reads tokens.
async fn observe_auth(
    binary: &Path,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Option<bool> {
    let mut command = Command::new(binary);
    command
        .args(["auth", "list"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.env_clear();
    for (key, value) in crate::filter_child_env(env.iter().cloned()) {
        command.env(key, value);
    }
    let child = crate::spawn_process_tree(&mut command).ok()?;
    let output = timeout(AUTH_TIMEOUT, child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut last: Option<u64> = None;
    for line in stdout.lines() {
        if let Some(count) = credentials_count(line) {
            last = Some(count);
        }
    }
    last.map(|count| count > 0)
}

fn credentials_count(line: &str) -> Option<u64> {
    let idx = line.find("credential")?;
    let before = line[..idx].trim_end();
    let number = before
        .split_whitespace()
        .next_back()?
        .chars()
        .filter(|ch| ch.is_ascii_digit())
        .collect::<String>();
    number.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opencode::parse::OpencodeStreamParser;
    use crate::opencode::session::{compose_serve_plan, session_create_body};
    use crate::{ApprovalDecision, HarnessEvent};
    use std::path::PathBuf;
    use tidebreak_core::PermissionMode;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/opencode/1.18.18")
    }

    fn replay(name: &str) -> (Vec<HarnessEvent>, u64) {
        let path = fixture_dir().join(format!("{name}.ndjson"));
        let input = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("missing fixture {}: {err}", path.display()));
        let out = OpencodeStreamParser::parse_ndjson(&input);
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
            "captured plugin/catalog broadcasts are recognized protocol state"
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

    /// `step-finish` reports one model call, and the parser keeps the most
    /// recent one. `approval-approve` ran two calls — 5267 then 123 fresh
    /// input — so the prompt still resident at the end is the second.
    #[test]
    fn context_tokens_are_the_last_step_prompt() {
        let (events, _) = replay("approval-approve");
        let usage = completed_usage(&events);
        assert_eq!(usage.context_tokens, 7_675);
        assert_eq!(usage.input_tokens, 123);
        assert_eq!(usage.cache_read_input_tokens, 7_552);
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
    fn fixture_replay_tool_use() {
        let (events, _) = replay("tool-use");
        // The `pending` part opens the call with `input: {}` and no start
        // time: the tool has not run yet. The `running` part carries the
        // assembled arguments, so the call starts named.
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolStarted {
                name,
                detail: tidebreak_core::ToolDetail::FileRead { path },
                ..
            } if name == "read" && path == "/workspace/README.md"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolCompleted {
                detail: Some(tidebreak_core::ToolDetail::FileRead { path }),
                ..
            } if path == "/workspace/README.md"
        )));
    }

    #[test]
    fn fixture_replay_approval_approve() {
        let (events, _) = replay("approval-approve");
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ApprovalRequested { raw, .. }
                if raw.get("permission") == Some(&serde_json::json!("bash"))
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ApprovalResolved {
                decision: ApprovalDecision::Approve,
                ..
            }
        )));
    }

    #[test]
    fn fixture_replay_approval_deny() {
        let (events, _) = replay("approval-deny");
        assert!(events
            .iter()
            .any(|event| matches!(event, HarnessEvent::ApprovalRequested { .. })));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ApprovalResolved {
                decision: ApprovalDecision::Deny { .. },
                ..
            }
        )));
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
    fn fixture_replay_error() {
        let (events, _) = replay("error");
        assert!(events
            .iter()
            .any(|event| matches!(event, HarnessEvent::TurnFailed { .. })));
    }

    #[test]
    fn adapter_has_a_fixtures_directory_with_a_manifest() {
        assert!(fixture_dir().join("manifest.toml").is_file());
    }

    #[test]
    fn capabilities_for_1_18_18_are_honest() {
        let caps = OpencodeAdapter::new().capabilities(&HarnessProbe {
            found: true,
            binary_path: None,
            version: Some("1.18.18".into()),
            authenticated: Some(true),
            stderr: String::new(),
            env: Vec::new(),
            commands: Vec::new(),
        });
        assert_eq!(caps.resume, CapLevel::Supported);
        assert_eq!(caps.streaming_deltas, CapLevel::Supported);
        assert_eq!(caps.structured_approvals, CapLevel::Supported);
        assert_eq!(caps.native_interrupt, CapLevel::Supported);
        assert_eq!(caps.plan_mode, CapLevel::Supported);
        assert_eq!(caps.auto_mode, CapLevel::Supported);
        assert_eq!(caps.allow_mode, CapLevel::Supported);
        assert_eq!(caps.mid_turn_steering, CapLevel::Unknown);
        assert_eq!(caps.reasoning_levels, CapLevel::Unsupported);
        assert_eq!(caps.native_file_change_events, CapLevel::Unknown);
        assert_eq!(caps.image_input, CapLevel::Unknown);
        assert_eq!(caps.slash_commands, CapLevel::Unknown);
    }

    #[test]
    fn launch_plan_never_includes_bypass_or_auto() {
        let plan = compose_serve_plan(
            Path::new("/usr/bin/opencode"),
            &[],
            Path::new("/workspace"),
            &[],
            &[],
            4096,
            None,
        )
        .unwrap();
        assert!(!plan
            .argv
            .iter()
            .any(|arg| arg.contains("dangerous") || arg == "--auto"));
        for mode in [
            PermissionMode::Plan,
            PermissionMode::Ask,
            PermissionMode::Auto,
        ] {
            let body = session_create_body(mode, None);
            assert_ne!(body.get("agent").and_then(|v| v.as_str()), Some(""));
            if let Some(rules) = body["permission"].as_array() {
                assert!(
                    !rules.iter().all(|rule| rule["action"] == "allow"),
                    "{mode} must not allow every permission"
                );
            }
        }
        let allow = session_create_body(PermissionMode::Allow, None);
        let rules = allow["permission"].as_array().unwrap();
        assert!(rules.iter().all(|rule| rule["action"] == "allow"));
    }

    #[test]
    fn credentials_count_reads_auth_list_footer() {
        assert_eq!(credentials_count("└  3 credentials"), Some(3));
        assert_eq!(credentials_count("└  0 credentials"), Some(0));
        assert!(observe_line_signed_in());
        assert!(!observe_line_signed_out());
    }

    fn observe_line_signed_in() -> bool {
        credentials_count("└  3 credentials")
            .map(|n| n > 0)
            .unwrap()
    }

    fn observe_line_signed_out() -> bool {
        credentials_count("└  0 credentials")
            .map(|n| n > 0)
            .unwrap()
    }
}
