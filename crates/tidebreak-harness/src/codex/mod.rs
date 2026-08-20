//! Codex CLI adapter. Secondary tier.
//!
//! Process model for 0.147.0: one long-lived `codex app-server --stdio`
//! JSON-RPC child per session. Chosen over `codex exec --json` because the
//! installed version's app-server handshake is stable and is the richer
//! approval channel (`item/commandExecution/requestApproval`).

pub mod parse;
pub mod session;

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tidebreak_core::{CapLevel, HarnessCaps, HarnessKind};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

use crate::codex::session::attach;
use crate::probe::{observe_version, probe_shell, HostEnv};
use crate::{
    filter_child_env, spawn_process_tree, HarnessAdapter, HarnessError, HarnessProbe,
    HarnessSession, ListedHarnessModel, SessionSpec,
};

const AUTH_TIMEOUT: Duration = Duration::from_secs(15);
const MODEL_LIST_TIMEOUT: Duration = Duration::from_secs(20);

/// Codex CLI adapter. Capabilities below are for the captured version
/// 0.147.0: verified flags are `Supported`/`Unsupported`; anything not
/// seen in a fixture is `Unknown`.
#[derive(Debug, Default, Clone, Copy)]
pub struct CodexAdapter;

impl CodexAdapter {
    /// Construct the adapter.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl HarnessAdapter for CodexAdapter {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Codex
    }

    async fn probe(&self, host: &HostEnv) -> HarnessProbe {
        match probe_shell(host, "codex").await {
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

    fn capabilities(&self, probe: &HarnessProbe) -> HarnessCaps {
        HarnessCaps {
            resume: CapLevel::Supported,
            streaming_deltas: CapLevel::Supported,
            structured_approvals: CapLevel::Supported,
            // The non-experimental `turn/steer` contract is verified for the
            // 0.147 line. Keep newer and older installations honest until
            // their generated schema is checked too.
            mid_turn_steering: if supports_native_steering(probe.version.as_deref()) {
                CapLevel::Supported
            } else {
                CapLevel::Unknown
            },
            plan_mode: CapLevel::Supported,
            // `thread/start` sandbox workspace-write + approvalPolicy
            // on-request; supervised by the captured approval channel.
            auto_mode: CapLevel::Supported,
            // `thread/start` sandbox danger-full-access + approvalPolicy never.
            allow_mode: CapLevel::Supported,
            reasoning_levels: CapLevel::Supported,
            native_file_change_events: CapLevel::Unknown,
            native_interrupt: CapLevel::Supported,
            image_input: CapLevel::Unknown,
            slash_commands: CapLevel::Unknown,
        }
    }

    async fn list_models(&self, probe: &HarnessProbe) -> Vec<ListedHarnessModel> {
        let Some(binary) = probe.binary_path.as_deref() else {
            return Vec::new();
        };
        list_app_server_models(binary, &probe.env).await
    }

    async fn launch(&self, spec: SessionSpec) -> Result<Box<dyn HarnessSession>, HarnessError> {
        if !spec.binary.is_absolute() {
            return Err(HarnessError::NotFound);
        }
        Ok(Box::new(attach(spec).await?))
    }
}

fn supports_native_steering(version: Option<&str>) -> bool {
    version
        .into_iter()
        .flat_map(str::split_whitespace)
        .filter_map(|candidate| {
            let candidate = candidate.strip_prefix('v').unwrap_or(candidate);
            let mut parts = candidate.split('.');
            let major = parts.next()?.parse::<u64>().ok()?;
            let minor = parts.next()?.parse::<u64>().ok()?;
            let patch = parts.next()?.parse::<u64>().ok()?;
            parts.next().is_none().then_some((major, minor, patch))
        })
        .is_some_and(|(major, minor, _)| major == 0 && minor == 147)
}

/// Ask the same app-server protocol a real session uses. Codex has no
/// `models` CLI subcommand, so the generic harness probe necessarily returns
/// an empty list even though the native picker has a catalog.
async fn list_app_server_models(
    binary: &Path,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Vec<ListedHarnessModel> {
    let mut command = Command::new(binary);
    command
        .args(["app-server", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear();
    for (key, value) in filter_child_env(env.iter().cloned()) {
        command.env(key, value);
    }
    let Ok(mut child) = spawn_process_tree(&mut command) else {
        return Vec::new();
    };
    let Some(mut stdin) = child.take_stdin() else {
        return Vec::new();
    };
    let Some(stdout) = child.take_stdout() else {
        return Vec::new();
    };
    let mut stdout = BufReader::new(stdout);

    if write_rpc(
        &mut stdin,
        &json!({
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": { "name": "tidebreak-harness", "version": "0.0.0" },
                "capabilities": { "experimentalApi": false }
            }
        }),
    )
    .await
    .is_err()
    {
        return Vec::new();
    }
    if timeout(MODEL_LIST_TIMEOUT, read_rpc_result(&mut stdout, 1))
        .await
        .ok()
        .flatten()
        .is_none()
    {
        return Vec::new();
    }
    if write_rpc(&mut stdin, &json!({ "method": "initialized" }))
        .await
        .is_err()
    {
        return Vec::new();
    }

    let mut listed = Vec::new();
    let mut cursor: Option<String> = None;
    for page in 0..4_i64 {
        let id = page + 2;
        if write_rpc(
            &mut stdin,
            &json!({
                "id": id,
                "method": "model/list",
                "params": {
                    "cursor": cursor,
                    "limit": 100,
                    "includeHidden": false
                }
            }),
        )
        .await
        .is_err()
        {
            break;
        }
        let Some(result) = timeout(MODEL_LIST_TIMEOUT, read_rpc_result(&mut stdout, id))
            .await
            .ok()
            .flatten()
        else {
            break;
        };
        listed.extend(parse_model_list(&result));
        cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    listed
}

async fn write_rpc(stdin: &mut tokio::process::ChildStdin, message: &Value) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    stdin.write_all(&bytes).await?;
    stdin.flush().await
}

async fn read_rpc_result<R: AsyncBufRead + Unpin>(reader: &mut R, id: i64) -> Option<Value> {
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        let message: Value = serde_json::from_str(&line).ok()?;
        if message.get("id").and_then(Value::as_i64) != Some(id) {
            continue;
        }
        return message.get("result").cloned();
    }
}

fn parse_model_list(result: &Value) -> Vec<ListedHarnessModel> {
    result
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|row| !row.get("hidden").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|row| {
            let id = row.get("model").or_else(|| row.get("id"))?.as_str()?.trim();
            if id.is_empty() {
                return None;
            }
            let label = row
                .get("displayName")
                .and_then(Value::as_str)
                .filter(|label| !label.trim().is_empty())
                .unwrap_or(id);
            Some(ListedHarnessModel {
                id: id.to_owned(),
                label: label.to_owned(),
                default: row
                    .get("isDefault")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

/// `codex login status` — "Logged in…" vs "Not logged in". Never reads tokens.
async fn observe_login(
    binary: &Path,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Option<bool> {
    let mut command = Command::new(binary);
    command
        .args(["login", "status"])
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
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    if line.starts_with("Logged in") {
        Some(true)
    } else if line.starts_with("Not logged in") {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::parse::CodexStreamParser;
    use crate::codex::session::{compose_app_server_plan, thread_start_policy};
    use crate::{ApprovalDecision, HarnessEvent};
    use std::path::PathBuf;
    use tidebreak_core::CodePermissionMode;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/codex/0.147.0")
    }

    fn replay(name: &str) -> (Vec<HarnessEvent>, u64) {
        let path = fixture_dir().join(format!("{name}.ndjson"));
        let input = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("missing fixture {}: {err}", path.display()));
        let out = CodexStreamParser::parse_ndjson(&input);
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
            "captured startup notifications are recognized protocol state"
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
    fn fixture_replay_hook_lifecycle() {
        let (events, unrecognized) = replay("hook-lifecycle");
        assert_eq!(unrecognized, 0, "hook lifecycle frames are known telemetry");
        assert!(events.is_empty(), "hooks do not change the transcript");
    }

    #[test]
    fn fixture_replay_tool_use() {
        let (events, _) = replay("tool-use");
        // `item/started` already carries the whole command, so the started
        // detail names its subject; `item/completed` repeats it.
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolStarted { name, detail, .. }
                if name == "commandExecution" && detail.specificity() > 0
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolCompleted {
                detail: Some(tidebreak_core::ToolDetail::Command { cmd, .. }),
                ..
            } if cmd.contains("note.txt")
        )));
    }

    #[test]
    fn fixture_replay_approval_approve() {
        let (events, _) = replay("approval-approve");
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ApprovalRequested { raw, .. }
                if raw.get("command").and_then(|value| value.as_str())
                    .is_some_and(|cmd| cmd.contains("python3"))
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
            .expect("resume fixture must report a thread id");
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
    fn capabilities_for_0_147_0_are_honest() {
        let caps = CodexAdapter::new().capabilities(&HarnessProbe {
            found: true,
            binary_path: None,
            version: Some("codex-cli 0.147.0".into()),
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
        assert_eq!(caps.reasoning_levels, CapLevel::Supported);
        assert_eq!(caps.image_input, CapLevel::Unknown);
        assert_eq!(caps.slash_commands, CapLevel::Unknown);
        assert_eq!(caps.mid_turn_steering, CapLevel::Supported);
        assert_eq!(caps.native_file_change_events, CapLevel::Unknown);
    }

    #[test]
    fn steering_capability_is_gated_to_the_verified_0_147_line() {
        let caps_for = |version: Option<&str>| {
            CodexAdapter::new()
                .capabilities(&HarnessProbe {
                    found: true,
                    binary_path: None,
                    version: version.map(str::to_owned),
                    authenticated: Some(true),
                    stderr: String::new(),
                    env: Vec::new(),
                    commands: Vec::new(),
                })
                .mid_turn_steering
        };

        assert_eq!(caps_for(Some("codex-cli 0.147.0")), CapLevel::Supported);
        assert_eq!(caps_for(Some("0.147.9")), CapLevel::Supported);
        assert_eq!(caps_for(Some("codex-cli 0.146.3")), CapLevel::Unknown);
        assert_eq!(caps_for(Some("codex-cli 0.148.0")), CapLevel::Unknown);
        assert_eq!(
            caps_for(Some("codex-cli 0.147.0-alpha.1")),
            CapLevel::Unknown
        );
        assert_eq!(caps_for(Some("development")), CapLevel::Unknown);
        assert_eq!(caps_for(None), CapLevel::Unknown);
    }

    #[test]
    fn native_model_catalog_uses_the_thread_start_token_and_hides_hidden_rows() {
        let listed = parse_model_list(&json!({
            "data": [
                {
                    "id": "catalog-row-1",
                    "model": "gpt-5.6-luna",
                    "displayName": "GPT-5.6-Luna",
                    "hidden": false,
                    "isDefault": true
                },
                {
                    "id": "catalog-row-2",
                    "model": "internal-model",
                    "displayName": "Internal",
                    "hidden": true,
                    "isDefault": false
                }
            ]
        }));
        assert_eq!(
            listed,
            vec![ListedHarnessModel {
                id: "gpt-5.6-luna".into(),
                label: "GPT-5.6-Luna".into(),
                default: true,
            }]
        );
    }

    #[test]
    fn launch_plan_never_includes_bypass_flag() {
        let plan = compose_app_server_plan(
            Path::new("/usr/bin/codex"),
            &[],
            Path::new("/workspace"),
            &[],
        )
        .unwrap();
        assert!(!plan
            .argv
            .iter()
            .any(|arg| arg.contains("dangerously-bypass")));
        for mode in [
            CodePermissionMode::Plan,
            CodePermissionMode::Ask,
            CodePermissionMode::Auto,
        ] {
            let (sandbox, approval) = thread_start_policy(mode);
            assert_ne!(sandbox, "danger-full-access");
            assert_ne!(approval, "never");
        }
        assert_eq!(
            thread_start_policy(CodePermissionMode::Allow),
            ("danger-full-access", "never")
        );
    }
}
