//! Grok CLI adapter. Best-effort tier.
//!
//! Process model for 1.0.4: one print-mode child per turn
//! (`--prompt-file` + `--output-format streaming-json`). Chosen over
//! `grok agent stdio` ACP because the captured print stream is a real
//! machine-readable NDJSON surface and no ACP permission
//! request/response pair was captured.

pub mod parse;
pub mod session;

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tidebreak_core::{CapLevel, HarnessCaps, HarnessKind, ReasoningEffort};
use tokio::process::Command;
use tokio::time::timeout;

use crate::grok::session::GrokSession;
use crate::probe::{observe_version, probe_shell, HostEnv};
use crate::{HarnessAdapter, HarnessError, HarnessProbe, HarnessSession, SessionSpec};

const AUTH_TIMEOUT: Duration = Duration::from_secs(15);

/// The ladder `grok --reasoning-effort` takes on the pinned 1.0.4. The CLI
/// names them itself when it refuses one, and it tops out below `max`.
pub(crate) const EFFORT_LADDER: &[ReasoningEffort] = &[
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::XHigh,
];

/// Grok CLI adapter. Capabilities below are for the captured version
/// 1.0.4: verified flags are `Supported`/`Unsupported`; anything not
/// seen in a fixture is `Unknown`.
#[derive(Debug, Default, Clone, Copy)]
pub struct GrokAdapter;

impl GrokAdapter {
    /// Construct the adapter.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl HarnessAdapter for GrokAdapter {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Grok
    }

    async fn probe(&self, host: &HostEnv) -> HarnessProbe {
        match probe_shell(host, "grok").await {
            Ok(capture) => {
                let version = observe_version(&capture.binary, &capture.env).await.ok();
                let authenticated = observe_login(&capture.binary, &capture.env).await;
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
            // Print-mode streaming-json has no parked approval request.
            // ACP stdio initialize/session/new was probed; no
            // request/response pair was captured.
            structured_approvals: CapLevel::Unsupported,
            mid_turn_steering: CapLevel::Unsupported,
            // `--permission-mode plan` and `--sandbox read-only` both
            // wrote files in captured 1.0.4 turns.
            plan_mode: CapLevel::Unsupported,
            // The default headless posture (no permission flags composed)
            // executed a write tool unprompted — re-probed 2026-08-17 on
            // 1.0.4 (d846eb93). Unsupervised: nothing escalates, which the
            // product states where the mode is chosen (decision 0038).
            auto_mode: CapLevel::Supported,
            // `--always-approve` accepted by the captured 1.0.4 CLI.
            allow_mode: CapLevel::Supported,
            // `--reasoning-effort` is documented and was used on capture.
            reasoning_levels: CapLevel::Supported,
            native_file_change_events: CapLevel::Unknown,
            native_interrupt: CapLevel::Supported,
            image_input: CapLevel::Unknown,
            slash_commands: CapLevel::Unknown,
        }
    }

    fn reasoning_efforts(&self, _probe: &HarnessProbe) -> Vec<ReasoningEffort> {
        EFFORT_LADDER.to_vec()
    }

    async fn list_models(&self, probe: &HarnessProbe) -> Vec<crate::ListedHarnessModel> {
        let Some(binary) = probe.binary_path.as_deref() else {
            return Vec::new();
        };
        crate::with_reasoning_efforts(
            crate::prefer_gateway_models(
                crate::list_cli_models(binary, &["models"], &probe.env).await,
            ),
            EFFORT_LADDER,
        )
    }

    async fn launch(&self, spec: SessionSpec) -> Result<Box<dyn HarnessSession>, HarnessError> {
        if !spec.binary.is_absolute() {
            return Err(HarnessError::NotFound);
        }
        crate::grok::session::refuse_unhonored_mode(spec.permission_mode)?;
        let version = observe_version(&spec.binary, &spec.env)
            .await
            .unwrap_or_else(|_| "unknown".into());
        Ok(Box::new(GrokSession::new(spec, version)))
    }
}

/// `grok models` — "You are logged in…" vs "You are not authenticated."
/// Never reads tokens.
async fn observe_login(
    binary: &Path,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Option<bool> {
    let mut command = Command::new(binary);
    command
        .args(["models"])
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
    login_status_from_models(&stdout)
}

fn login_status_from_models(stdout: &str) -> Option<bool> {
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    if line.starts_with("You are logged in") {
        Some(true)
    } else if line.starts_with("You are not authenticated") {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grok::parse::GrokStreamParser;
    use crate::grok::session::{compose_print_plan, refuse_unhonored_mode, PrintLaunch};
    use crate::HarnessEvent;
    use std::path::{Path, PathBuf};
    use tidebreak_core::PermissionMode;

    fn fixture_dir(version: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("fixtures/grok/{version}"))
    }

    fn replay(name: &str) -> (Vec<HarnessEvent>, u64) {
        replay_version("1.0.4", name)
    }

    fn replay_version(version: &str, name: &str) -> (Vec<HarnessEvent>, u64) {
        let directory = fixture_dir(version);
        let path = directory.join(format!("{name}.ndjson"));
        let input = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("missing fixture {}: {err}", path.display()));
        let mut parser = GrokStreamParser::new();
        parser.set_version(version);
        let mut events = Vec::new();
        for line in input.lines() {
            events.extend(parser.push_line(line));
        }
        let unrecognized = parser.unrecognized();
        let expected_path = directory.join(format!("{name}.expected.json"));
        if std::env::var_os("UPDATE_HARNESS_FIXTURES").is_some() {
            let rendered = format!("{}\n", serde_json::to_string_pretty(&events).unwrap());
            std::fs::write(&expected_path, rendered).unwrap();
        } else {
            let expected = std::fs::read_to_string(&expected_path).unwrap_or_else(|err| {
                panic!(
                    "missing expected sequence {}: {err}; regenerate with \
                     UPDATE_HARNESS_FIXTURES=1 cargo test -p tidebreak-harness",
                    expected_path.display()
                )
            });
            let actual = format!("{}\n", serde_json::to_string_pretty(&events).unwrap());
            assert_eq!(
                expected.replace("\r\n", "\n"),
                actual,
                "normalized sequence for {name} drifted from the fixture"
            );
        }
        (events, unrecognized)
    }

    #[test]
    fn fixture_replay_plain_text() {
        let (events, unrecognized) = replay("plain-text");
        assert_eq!(
            unrecognized, 0,
            "the captured available-command inventory is recognized metadata"
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
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolStarted { name, .. } if name == "read_file"
        )));
        assert!(events
            .iter()
            .any(|event| matches!(event, HarnessEvent::ToolCompleted { .. })));
    }

    /// Grok publishes both shapes: a per-call `usage` event and a cumulative
    /// `end` payload. The spend counts take the cumulative one; occupancy
    /// takes the last per-call event.
    ///
    /// `permission-denied` ran three calls. Its `end` payload sums to 54,748
    /// prompt-side tokens while the prompt still resident was 18,311 — a 3x
    /// over-read, and enough to clamp a ring that should read a third full.
    #[test]
    fn context_tokens_are_the_last_call_not_the_cumulative_end() {
        let (events, _) = replay("permission-denied");
        let usage = completed_usage(&events);
        assert_eq!(usage.context_tokens, 18_311);
        let spend =
            usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens;
        assert_eq!(spend, 54_748);
    }

    /// A single-call turn has nothing to disagree about: the one `usage`
    /// event and the `end` payload report the same prompt.
    #[test]
    fn a_single_call_turn_reads_the_same_prompt_either_way() {
        let (events, _) = replay("plain-text");
        let usage = completed_usage(&events);
        assert_eq!(usage.context_tokens, 18_150);
        assert_eq!(
            usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens,
            18_150
        );
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
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolCompleted { outcome, preview, .. }
                if *outcome == tidebreak_core::ToolOutcome::Denied
                    && preview.contains("Denied by permission policy")
        )));
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
        assert_eq!(started, "01a00759-bacb-7d63-a3fb-515ad8d7c292");
    }

    #[test]
    fn fixture_replay_interrupt() {
        let (events, unrecognized) = replay("interrupt");
        assert_eq!(
            unrecognized, 0,
            "the captured available-command inventory is recognized metadata"
        );
        assert!(events
            .iter()
            .any(|event| matches!(event, HarnessEvent::ReasoningDelta { .. })));
        assert!(events.iter().all(|event| {
            !matches!(
                event,
                HarnessEvent::TurnCompleted { .. } | HarnessEvent::TurnFailed { .. }
            )
        }));
    }

    #[test]
    fn fixture_replay_error() {
        let (events, _) = replay("error");
        assert!(events
            .iter()
            .any(|event| matches!(event, HarnessEvent::TurnFailed { .. })));
    }

    #[test]
    fn fixture_replay_subagent_task() {
        let subagent_id = "01a02025-bcce-7723-a8f6-27e6f2a6a856";
        let (events, unrecognized) = replay_version("1.0.5", "subagent-task");
        assert_eq!(unrecognized, 0);

        let task_start = events.iter().position(|event| {
            matches!(
                event,
                HarnessEvent::ToolStarted {
                    call_id,
                    name,
                    parent_call_id: None,
                    ..
                } if call_id == subagent_id && name == "Task"
            )
        });
        let running_poll = events.iter().position(|event| {
            matches!(
                event,
                HarnessEvent::ToolCompleted {
                    call_id,
                    parent_call_id: Some(parent_call_id),
                    preview,
                    ..
                } if call_id == &format!("call-output-1:{subagent_id}")
                    && parent_call_id == subagent_id
                    && preview.contains("still running")
            )
        });
        let child_output = events.iter().position(|event| {
            matches!(
                event,
                HarnessEvent::AssistantMessage {
                    text,
                    parent_call_id: Some(parent_call_id),
                } if parent_call_id == subagent_id && text == "Focused parser checks passed."
            )
        });
        let task_end = events.iter().position(|event| {
            matches!(
                event,
                HarnessEvent::ToolCompleted {
                    call_id,
                    outcome: tidebreak_core::ToolOutcome::Succeeded,
                    parent_call_id: None,
                    ..
                } if call_id == subagent_id
            )
        });

        assert!(task_start < running_poll);
        assert!(running_poll < child_output);
        assert!(child_output < task_end);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    HarnessEvent::ToolCompleted {
                        call_id,
                        parent_call_id: None,
                        ..
                    } if call_id == subagent_id
                ))
                .count(),
            1,
            "a running poll must not settle the spanning Task"
        );
    }

    #[test]
    fn adapter_has_a_fixtures_directory_with_a_manifest() {
        assert!(fixture_dir("1.0.4").join("manifest.toml").is_file());
        assert!(fixture_dir("1.0.5").join("manifest.toml").is_file());
    }

    #[test]
    fn checked_in_stop_reasons_are_allowlisted() {
        for version in ["1.0.4", "1.0.5"] {
            for entry in std::fs::read_dir(fixture_dir(version)).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().and_then(|extension| extension.to_str()) != Some("ndjson") {
                    continue;
                }
                let input = std::fs::read_to_string(&path).unwrap();
                for line in input.lines() {
                    let value: serde_json::Value = serde_json::from_str(line).unwrap();
                    if value.get("type").and_then(serde_json::Value::as_str) != Some("end") {
                        continue;
                    }
                    let reason = value
                        .get("stopReason")
                        .and_then(serde_json::Value::as_str)
                        .expect("captured Grok terminal must name its stop reason");
                    assert!(
                        ["end_turn", "cancelled"].contains(&reason),
                        "{} has unknown stop reason {reason}",
                        path.display()
                    );
                }
            }
        }
    }

    #[test]
    fn capabilities_for_1_0_4_are_honest() {
        let caps = GrokAdapter::new().capabilities(&HarnessProbe {
            found: true,
            binary_path: None,
            version: Some("grok 1.0.4 (d846eb93d94d) [stable]".into()),
            authenticated: Some(true),
            stderr: String::new(),
            env: Vec::new(),
            commands: Vec::new(),
        });
        assert_eq!(caps.resume, CapLevel::Supported);
        assert_eq!(caps.streaming_deltas, CapLevel::Supported);
        assert_eq!(caps.native_interrupt, CapLevel::Supported);
        assert_eq!(caps.reasoning_levels, CapLevel::Supported);
        assert_eq!(caps.structured_approvals, CapLevel::Unsupported);
        assert_eq!(caps.plan_mode, CapLevel::Unsupported);
        assert_eq!(caps.auto_mode, CapLevel::Supported);
        assert_eq!(caps.allow_mode, CapLevel::Supported);
        assert_eq!(caps.image_input, CapLevel::Unknown);
        assert_eq!(caps.slash_commands, CapLevel::Unknown);
        assert_eq!(caps.mid_turn_steering, CapLevel::Unsupported);
        assert_eq!(caps.native_file_change_events, CapLevel::Unknown);
    }

    #[test]
    fn auto_and_allow_are_honored_and_plan_and_ask_are_refused() {
        assert!(refuse_unhonored_mode(PermissionMode::Auto).is_ok());
        assert!(refuse_unhonored_mode(PermissionMode::Allow).is_ok());
        for mode in [PermissionMode::Plan, PermissionMode::Ask] {
            let err = refuse_unhonored_mode(mode).unwrap_err();
            assert!(matches!(err, HarnessError::PermissionModeUnsupported(m) if m == mode));
        }
    }

    #[test]
    fn auto_launch_plan_never_includes_bypass_flags() {
        let plan = compose_print_plan(PrintLaunch {
            binary: Path::new("/usr/bin/grok"),
            extra_argv: &[],
            cwd: Path::new("/workspace"),
            extra_env: &[],
            relay_auth: None,
            relay_key_env: None,
            resume_ref: None,
            prompt_file: Path::new("/tmp/prompt.txt"),
            mode: PermissionMode::Auto,
            model: None,
            effort: None,
        })
        .unwrap();
        assert!(!plan.argv.iter().any(|arg| {
            arg.contains("always-approve")
                || arg.contains("yolo")
                || arg.contains("bypass")
                || arg.contains("dangerous")
        }));
        assert!(!plan.argv.iter().any(|arg| arg == "--permission-mode"));
        assert!(!plan.argv.iter().any(|arg| arg == "--deny"));
        assert_eq!(
            plan.argv
                .windows(2)
                .find(|pair| pair[0] == "--output-format")
                .map(|pair| pair[1].as_str()),
            Some("streaming-json")
        );
        assert!(!plan.argv.iter().any(|arg| arg.contains("hello from")));
    }

    #[test]
    fn allow_launch_plan_composes_always_approve() {
        let plan = compose_print_plan(PrintLaunch {
            binary: Path::new("/usr/bin/grok"),
            extra_argv: &[],
            cwd: Path::new("/workspace"),
            extra_env: &[],
            relay_auth: None,
            relay_key_env: None,
            resume_ref: None,
            prompt_file: Path::new("/tmp/prompt.txt"),
            mode: PermissionMode::Allow,
            model: None,
            effort: None,
        })
        .unwrap();
        assert!(plan.argv.iter().any(|arg| arg == "--always-approve"));
    }

    #[test]
    fn login_status_reads_models_header() {
        assert_eq!(
            login_status_from_models("You are logged in with grok.com.\n"),
            Some(true)
        );
        assert_eq!(
            login_status_from_models("You are not authenticated.\n"),
            Some(false)
        );
        assert_eq!(login_status_from_models("something else\n"), None);
    }
}
