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
use tidebreak_core::{CapLevel, HarnessCaps, HarnessKind, ReasoningEffort};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

use crate::codex::session::CodexSession;
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
                let version = observe_version(&capture.binary, &capture.env)
                    .await
                    .ok()
                    .map(|version| normalize_codex_version(&version));
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
        // The app-server child spawns on the first turn, not here (decision
        // 0064), so attaching a session costs no engine runtime.
        Ok(Box::new(CodexSession::new(spec)))
    }
}

fn normalize_codex_version(version: &str) -> String {
    version
        .split_whitespace()
        .find_map(|candidate| {
            let candidate = candidate.strip_prefix('v').unwrap_or(candidate);
            candidate
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_digit())
                .then(|| candidate.to_owned())
        })
        .unwrap_or_else(|| version.trim().to_owned())
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
        .next()
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
                reasoning_efforts: parse_reasoning_efforts(row),
                fast_mode: parse_fast_mode(row),
            })
        })
        .collect()
}

/// The effort ladder one `model/list` row advertises, ascending.
///
/// Codex states this per model rather than per engine: a gateway row and its
/// own rows do not offer the same rungs, and only some reach `ultra`. A token
/// this build has no level for is dropped, so a newer catalog cannot make the
/// picker offer something nothing downstream can spell.
/// Whether a catalog row advertises Codex's fast speed tier.
///
/// Codex states this two ways and the spelling varies by surface: the model
/// catalog lists `additional_speed_tiers` (`["fast"]`), while the richer
/// `service_tiers` array carries the id the request actually sends
/// (`priority`). Read either, and accept both camelCase and snake_case, since
/// the app-server and the packaged catalog do not agree on case.
///
/// A row that advertises neither reads as no fast mode. That is the same
/// conservative direction the effort ladder takes: an unstated capability
/// hides the control rather than offering one the engine would ignore.
fn parse_fast_mode(row: &Value) -> bool {
    const FAST_TIERS: &[&str] = &["fast", "priority"];
    let names = |key: &str, alt: &str| -> Option<&Value> { row.get(key).or_else(|| row.get(alt)) };
    let speed_tiers = names("additional_speed_tiers", "additionalSpeedTiers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str);
    let service_tiers = names("service_tiers", "serviceTiers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tier| {
            tier.get("id")
                .and_then(Value::as_str)
                .or_else(|| tier.as_str())
        });
    speed_tiers
        .chain(service_tiers)
        .any(|tier| FAST_TIERS.contains(&tier.trim()))
}

fn parse_reasoning_efforts(row: &Value) -> Vec<ReasoningEffort> {
    let mut levels: Vec<ReasoningEffort> = row
        .get("supportedReasoningEfforts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|option| {
            option
                .get("reasoningEffort")
                .and_then(Value::as_str)
                .or_else(|| option.as_str())
        })
        .filter_map(|token| ReasoningEffort::from_str(token.trim()))
        .collect();
    levels.sort_unstable();
    levels.dedup();
    levels
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
    login_status_from_output(&output.stdout, &output.stderr)
}

/// The first status-shaped line from either stream, stdout first.
///
/// The pinned 0.147 writes the status line to stderr under the probe's spawn
/// shape (null stdin, piped stdout), so neither stream alone is authoritative:
/// a stdout-only read reported "not observed" on signed-in machines. A
/// non-status first line on one stream does not hide a status line on the
/// other.
fn login_status_from_output(stdout: &[u8], stderr: &[u8]) -> Option<bool> {
    let first_line = |bytes: &[u8]| -> String {
        let text = String::from_utf8_lossy(bytes);
        text.lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("")
            .to_owned()
    };
    for line in [first_line(stdout), first_line(stderr)] {
        if line.starts_with("Logged in") {
            return Some(true);
        }
        if line.starts_with("Not logged in") {
            return Some(false);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::parse::CodexStreamParser;
    use crate::codex::session::{compose_app_server_plan, thread_start_policy};
    use crate::{ApprovalDecision, HarnessEvent};
    use std::path::PathBuf;
    use tidebreak_core::PermissionMode;

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

    /// `tokenUsage` reports `total` (the turn) beside `last` (the call that
    /// just finished). The spend counts read `total`; occupancy reads `last`,
    /// whose raw `inputTokens` is the whole prompt that call sent.
    #[test]
    fn context_tokens_come_from_the_last_call() {
        let (events, _) = replay("approval-approve");
        let usage = completed_usage(&events);
        assert_eq!(usage.context_tokens, 14_466);
        let spend =
            usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens;
        assert_eq!(spend, 28_794);
        assert!(usage.context_tokens < spend);
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

    /// One app-server connection carries the parent thread and every collab
    /// subagent it spawns. This capture is the only fixture where that
    /// happens, so it pins both halves: the child's work lands under its
    /// `Task` span, and the child's turn and counters leave the parent's
    /// alone (decision 52).
    #[test]
    fn fixture_replay_collab_agents() {
        const CHILD: &str = "01a039e7-283b-7213-b24c-3378305ba24d";
        let (events, unrecognized) = replay("collab-agents");
        assert_eq!(unrecognized, 0, "collab frames are known protocol state");

        let spans = events
            .iter()
            .filter(
                |event| matches!(event, HarnessEvent::ToolStarted { name, .. } if name == "Task"),
            )
            .collect::<Vec<_>>();
        assert_eq!(spans.len(), 1, "the capture spawns one subagent");
        assert!(matches!(
            spans[0],
            HarnessEvent::ToolStarted {
                call_id,
                detail,
                parent_call_id: None,
                ..
            } if call_id == CHILD && detail.subject().contains("cat note.txt")
        ));

        // The child publishes its own items on its own thread, so the span
        // needs no correlation table to claim them.
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolStarted {
                name,
                parent_call_id: Some(parent),
                ..
            } if name == "commandExecution" && parent == CHILD
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolStarted {
                name,
                parent_call_id: Some(parent),
                ..
            } if name == "WaitAgent" && parent == CHILD
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::AssistantMessage {
                text,
                parent_call_id: Some(parent),
            } if text == "alpha" && parent == CHILD
        )));

        // `agentsStates` repeats a terminal state on every later collab call,
        // and the child's own `turn/completed` reports it again.
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    HarnessEvent::ToolCompleted { call_id, .. } if call_id == CHILD
                ))
                .count(),
            1,
            "a repeated terminal state settles the span once"
        );

        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, HarnessEvent::TurnStarted))
                .count(),
            1,
            "the child's turn must not start a second Tidebreak turn"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, HarnessEvent::TurnCompleted { .. }))
                .count(),
            1,
            "the child's turn must not close the parent's turn"
        );
        // A transient delta carries no attribution, so the child's is dropped
        // and only the parent's reaches the transcript.
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    HarnessEvent::AssistantDelta { text } if text == "alpha"
                ))
                .count(),
            1,
            "the child's delta must not stream into the parent transcript"
        );

        // The parent's counters, not the sum of both threads'.
        let usage = completed_usage(&events);
        assert_eq!(usage.input_tokens, 17_612);
        assert_eq!(usage.cache_read_input_tokens, 89_856);
        assert_eq!(usage.context_tokens, 23_449);
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
        assert_eq!(
            completed_usage(&events),
            tidebreak_core::CodeUsage {
                input_tokens: 1_272,
                output_tokens: 6,
                cache_read_input_tokens: 14_080,
                cache_creation_input_tokens: 0,
                context_tokens: 15_352,
                first_call_context_tokens: Some(15_352),
            },
            "the resumed turn must not inherit the first turn's spend"
        );
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
    fn posture_retry_fixture_repeats_the_same_policy_after_rejection() {
        let (events, unrecognized) = replay("posture-retry");
        assert_eq!(unrecognized, 0);
        assert!(matches!(
            events.first(),
            Some(HarnessEvent::TurnFailed { .. })
        ));
        assert!(matches!(
            events.last(),
            Some(HarnessEvent::TurnCompleted { .. })
        ));

        let input = std::fs::read_to_string(fixture_dir().join("posture-retry.ndjson")).unwrap();
        let requests = input
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter_map(|value| value.get("msg").cloned())
            .filter(|message| {
                message.get("method").and_then(serde_json::Value::as_str) == Some("turn/start")
            })
            .collect::<Vec<_>>();

        assert_eq!(requests.len(), 2);
        for request in requests {
            assert_eq!(request["params"]["sandboxPolicy"]["type"], "readOnly");
            assert_eq!(request["params"]["approvalPolicy"], "untrusted");
        }
    }

    #[test]
    fn checked_in_terminal_values_are_allowlisted() {
        for entry in std::fs::read_dir(fixture_dir()).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("ndjson") {
                continue;
            }
            let input = std::fs::read_to_string(&path).unwrap();
            for line in input.lines() {
                let value: serde_json::Value = serde_json::from_str(line).unwrap();
                let message = value.get("msg").unwrap_or(&value);
                match message.get("method").and_then(serde_json::Value::as_str) {
                    Some("turn/completed") => {
                        let status = message
                            .pointer("/params/turn/status")
                            .and_then(serde_json::Value::as_str)
                            .expect("captured turn terminal must name its status");
                        assert!(
                            ["completed", "interrupted", "failed"].contains(&status),
                            "{} has unknown turn status {status}",
                            path.display()
                        );
                    }
                    Some("item/completed") => {
                        let item = &message["params"]["item"];
                        let item_type = item.get("type").and_then(serde_json::Value::as_str);
                        if matches!(
                            item_type,
                            Some("commandExecution" | "fileChange" | "collabAgentToolCall")
                        ) {
                            let status = item
                                .get("status")
                                .and_then(serde_json::Value::as_str)
                                .expect("captured tool terminal must name its status");
                            assert!(
                                [
                                    "completed",
                                    "declined",
                                    "failed",
                                    "interrupted",
                                    "cancelled",
                                    "canceled"
                                ]
                                .contains(&status),
                                "{} has unknown {item_type:?} status {status}",
                                path.display()
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn adapter_has_a_fixtures_directory_with_a_manifest() {
        assert!(fixture_dir().join("manifest.toml").is_file());
    }

    #[test]
    fn codex_versions_use_the_bare_engine_version() {
        assert_eq!(normalize_codex_version("codex-cli 0.147.0"), "0.147.0");
        assert_eq!(normalize_codex_version("codex-cli v0.147.0"), "0.147.0");
        assert_eq!(normalize_codex_version("0.147.0"), "0.147.0");
    }

    #[test]
    fn login_status_reads_the_stream_the_cli_writes() {
        // The pinned 0.147 writes `login status` to stderr when stdin is not
        // a terminal — the probe's spawn shape — so a stdout-only read
        // reported "not observed" on signed-in machines.
        assert_eq!(
            login_status_from_output(b"", b"Logged in using ChatGPT\n"),
            Some(true)
        );
        assert_eq!(
            login_status_from_output(b"", b"Not logged in\n"),
            Some(false)
        );
        assert_eq!(
            login_status_from_output(b"Logged in using ChatGPT\n", b""),
            Some(true)
        );
        assert_eq!(
            login_status_from_output(b"Not logged in\n", b""),
            Some(false)
        );
        // stdout stays authoritative when both streams answer.
        assert_eq!(
            login_status_from_output(b"Not logged in\n", b"Logged in\n"),
            Some(false)
        );
        // A non-status first line on one stream does not hide the other's.
        assert_eq!(
            login_status_from_output(b"warning: update available\n", b"Logged in using ChatGPT\n"),
            Some(true)
        );
        assert_eq!(login_status_from_output(b"", b""), None);
        assert_eq!(
            login_status_from_output(b"something else\n", b"also something\n"),
            None
        );
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
                    "isDefault": true,
                    "defaultReasoningEffort": "medium",
                    "supportedReasoningEfforts": [
                        {"reasoningEffort": "high", "description": ""},
                        {"reasoningEffort": "low", "description": ""},
                        {"reasoningEffort": "ultra", "description": ""},
                        {"reasoningEffort": "sideways", "description": ""}
                    ]
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
                // Ascending, and a token this build cannot spell is dropped.
                reasoning_efforts: vec![
                    ReasoningEffort::Low,
                    ReasoningEffort::High,
                    ReasoningEffort::Ultra,
                ],
                // The fixture row advertises no speed tier.
                fast_mode: false,
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
            None,
        )
        .unwrap();
        assert!(!plan
            .argv
            .iter()
            .any(|arg| arg.contains("dangerously-bypass")));
        for mode in [
            PermissionMode::Plan,
            PermissionMode::Ask,
            PermissionMode::Auto,
        ] {
            let (sandbox, approval) = thread_start_policy(mode);
            assert_ne!(sandbox, "danger-full-access");
            assert_ne!(approval, "never");
        }
        assert_eq!(
            thread_start_policy(PermissionMode::Allow),
            ("danger-full-access", "never")
        );
    }
}
