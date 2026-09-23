//! Grok CLI adapter. Best-effort tier.
//!
//! Pins 1.0.4 and 1.0.5 use one print-mode child per turn. The captured
//! 1.0.13 ACP channel carries native approvals, cancellation, and session
//! loading over stdin/stdout. Other versions retain the print fallback
//! without claiming a verified Auto posture.
//!
//! Grok print/ACP protocols have no extra-read-root flag. `allowed_read_roots`
//! are still required to be absolute so a relative private path cannot slip
//! through; the engine then runs without additional read scoping rather than
//! dropping the field silently.

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

/// Grok's switch that turns off its own update check for one process
/// (documented in the 1.0.13 binary). Tidebreak runs one exact release, so
/// every Grok child it starts sets it.
pub const DISABLE_AUTOUPDATER_ENV: &str = "GROK_DISABLE_AUTOUPDATER";

/// Longest line read while waiting for the ACP `initialize` answer. It lists
/// models and commands, not history, so this is generous.
const INITIALIZE_MAX_LINE: usize = 1_024 * 1_024;

/// The original `grok --reasoning-effort` ladder captured on 1.0.4.
pub(crate) const EFFORT_LADDER_1_0_4: &[ReasoningEffort] = &[
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::XHigh,
];

/// Grok 1.0.5's CLI flag enum dropped `medium` and `xhigh` and added `max`.
/// That pin is the only release captured with this vocabulary.
pub(crate) const EFFORT_LADDER_1_0_5: &[ReasoningEffort] = &[
    ReasoningEffort::Low,
    ReasoningEffort::High,
    ReasoningEffort::Max,
];

/// Current `grok --reasoning-effort` vocabulary. Later CLIs restored
/// `medium` and `xhigh`. `max` remains a CLI flag for non-Grok models the
/// engine can host (for example GLM); Grok chat models do not take it.
pub(crate) const EFFORT_LADDER_CURRENT: &[ReasoningEffort] = &[
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::XHigh,
    ReasoningEffort::Max,
];

/// xAI's published Grok chat ladder. `max` is not a Grok model option.
const GROK_MODEL_EFFORTS: &[ReasoningEffort] = &[
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::XHigh,
];

pub(super) fn effort_ladder_for_version(version: Option<&str>) -> &'static [ReasoningEffort] {
    match crate::probe::version_patch_line(version) {
        Some((1, 0, 5)) => EFFORT_LADDER_1_0_5,
        Some(version) if version >= (1, 0, 6) => EFFORT_LADDER_CURRENT,
        _ => EFFORT_LADDER_1_0_4,
    }
}

fn effort_ladder(probe: &HarnessProbe) -> &'static [ReasoningEffort] {
    effort_ladder_for_version(probe.version.as_deref())
}

/// The levels one listed model may actually take on this CLI version.
///
/// Grok-family ids intersect the engine flag enum with xAI's published
/// ladder so the picker never offers `max` for Grok 4.7. Other models keep
/// the engine ladder, which is how GLM still reaches `max`.
pub(crate) fn effective_effort_ladder(
    version: Option<&str>,
    model: Option<&str>,
) -> Vec<ReasoningEffort> {
    let engine = effort_ladder_for_version(version);
    match model.and_then(published_grok_model_efforts) {
        Some(published) => engine
            .iter()
            .copied()
            .filter(|effort| published.contains(effort))
            .collect(),
        None => engine.to_vec(),
    }
}

pub(crate) fn clamp_effort(
    version: Option<&str>,
    model: Option<&str>,
    effort: ReasoningEffort,
) -> Option<ReasoningEffort> {
    effort.clamp_to(&effective_effort_ladder(version, model))
}

fn published_grok_model_efforts(model_id: &str) -> Option<&'static [ReasoningEffort]> {
    let leaf = model_id.rsplit(['/', ':']).next().unwrap_or(model_id);
    leaf.starts_with("grok-").then_some(GROK_MODEL_EFFORTS)
}

fn with_model_reasoning_efforts(
    models: Vec<crate::ListedHarnessModel>,
    version: Option<&str>,
    reported: Option<&crate::ReportedEfforts>,
) -> Vec<crate::ListedHarnessModel> {
    models
        .into_iter()
        .map(|mut model| {
            // The engine's own statement for this model wins; the tables are
            // the fallback for a model or a version it says nothing about.
            model.reasoning_efforts = reported
                .and_then(|reported| reported.models.get(&model.id))
                .cloned()
                .unwrap_or_else(|| effective_effort_ladder(version, Some(&model.id)));
            model
        })
        .collect()
}

/// Each model's effort ladder, from the engine's ACP `initialize` result.
///
/// Grok lists its models under `_meta.modelState.availableModels`, each with
/// `_meta.reasoningEfforts` naming the levels it takes (captured on 1.0.13).
/// A level Tidebreak cannot send is left out. A model that says it takes no
/// effort gets an empty ladder; one that states no ladder at all, as a row
/// from a relay's model list can, is left to the tables.
pub(crate) fn model_efforts_from_initialize(
    result: &serde_json::Value,
) -> std::collections::BTreeMap<String, Vec<ReasoningEffort>> {
    let mut models = std::collections::BTreeMap::new();
    let listed = result
        .pointer("/_meta/modelState/availableModels")
        .and_then(serde_json::Value::as_array);
    for model in listed.into_iter().flatten() {
        let Some(id) = model
            .get("modelId")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.trim().is_empty())
        else {
            continue;
        };
        let meta = model.get("_meta");
        let takes_effort = meta
            .and_then(|meta| meta.get("supportsReasoningEffort"))
            .and_then(serde_json::Value::as_bool);
        let stated = meta
            .and_then(|meta| meta.get("reasoningEfforts"))
            .and_then(serde_json::Value::as_array);
        let mut ladder: Vec<ReasoningEffort> = match (takes_effort, stated) {
            (Some(false), _) => Vec::new(),
            (_, Some(levels)) => levels
                .iter()
                .filter_map(|level| level.get("value").cloned())
                .filter_map(|value| serde_json::from_value(value).ok())
                .collect(),
            (_, None) => continue,
        };
        ladder.sort_unstable();
        ladder.dedup();
        models.insert(id.to_owned(), ladder);
    }
    models
}

/// Ask the engine for each model's effort ladder.
///
/// Only versions whose ACP channel is captured are asked. The child answers
/// `initialize` without a session or a sign-in, and is stopped as soon as it
/// has. `None` when the version has no ACP channel or the answer names no
/// model, so the tables apply.
async fn observe_model_efforts(
    binary: &Path,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
    version: Option<&str>,
) -> Option<crate::ReportedEfforts> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    if !version.is_some_and(session::supports_acp_version) {
        return None;
    }
    let mut command = Command::new(binary);
    command
        .args(["agent", "--no-leader", "stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // The probe belongs to no repository, so no project config is found.
    let neutral = tempfile::tempdir().ok()?;
    command.current_dir(neutral.path());
    command.env_clear();
    for (key, value) in crate::filter_child_env(env.iter().cloned()) {
        command.env(key, value);
    }
    command.env(DISABLE_AUTOUPDATER_ENV, "1");
    let mut child = crate::spawn_process_tree(&mut command).ok()?;
    let (Some(mut stdin), Some(mut stdout)) = (child.take_stdin(), child.take_stdout()) else {
        let _ = child.terminate().await;
        return None;
    };
    let answer = timeout(AUTH_TIMEOUT, async {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": 1, "clientCapabilities": {}},
        });
        stdin
            .write_all(format!("{request}\n").as_bytes())
            .await
            .ok()?;
        stdin.flush().await.ok()?;
        let budget = crate::StreamBudget {
            max_partial_line: INITIALIZE_MAX_LINE,
            ..crate::StreamBudget::default()
        };
        let mut lines = crate::StreamLineBuffer::new();
        let mut chunk = vec![0_u8; budget.chunk_size];
        loop {
            let read = stdout.read(&mut chunk).await.ok()?;
            if read == 0 {
                return None;
            }
            for line in lines.push(&chunk[..read], budget).lines {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&line.text) else {
                    continue;
                };
                if value.get("id") == Some(&serde_json::json!(1)) && value.get("method").is_none() {
                    return value.get("result").cloned();
                }
            }
        }
    })
    .await
    .ok()
    .flatten();
    let _ = child.terminate().await;
    let models = model_efforts_from_initialize(&answer?);
    (!models.is_empty()).then(|| crate::ReportedEfforts {
        engine: Vec::new(),
        models,
    })
}

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
                let version = match host.declared_version(HarnessKind::Grok) {
                    Some(declared) => Some(declared.to_owned()),
                    None => observe_version(&capture.binary, &capture.env).await.ok(),
                };
                let (authenticated, reported_efforts) = tokio::join!(
                    observe_login(&capture.binary, &capture.env),
                    observe_model_efforts(&capture.binary, &capture.env, version.as_deref()),
                );
                HarnessProbe {
                    found: true,
                    binary_path: Some(capture.binary),
                    version,
                    authenticated,
                    stderr: capture.stderr,
                    env: capture.env,
                    commands: Vec::new(),
                    reported_efforts,
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
                reported_efforts: None,
            },
        }
    }

    fn capabilities(&self, probe: &HarnessProbe) -> HarnessCaps {
        let mut caps = HarnessCaps {
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
            // Tidebreak contract concepts, not protocol surface (decisions
            // 0033, 0048): no external engine parks durably, takes
            // structured answers, or mints standing grants.
            durable_parks: CapLevel::Unsupported,
            user_questions: CapLevel::Unsupported,
            standing_grants: CapLevel::Unsupported,
            mid_turn_resume: CapLevel::Unsupported,
            transcript: CapLevel::Unsupported,
            memory_loopback: CapLevel::Unsupported,
        };
        // An unprompted write on an older pin does not establish a later
        // release's default posture. ACP support is enabled separately below.
        let captured_print_auto = probe.version.as_deref().is_some_and(|version| {
            let version = version
                .trim()
                .strip_prefix("grok ")
                .unwrap_or(version.trim());
            matches!(version.split_whitespace().next(), Some("1.0.4" | "1.0.5"))
        });
        if !captured_print_auto {
            caps.auto_mode = CapLevel::Unknown;
        }
        if crate::probe::off_pinned_line(probe.version.as_deref(), (1, 0)) {
            caps.structured_approvals = CapLevel::Unknown;
            caps.plan_mode = CapLevel::Unknown;
        }
        if probe
            .version
            .as_deref()
            .is_some_and(session::supports_acp_version)
        {
            caps.structured_approvals = CapLevel::Supported;
            caps.auto_mode = CapLevel::Supported;
            caps.image_input = CapLevel::Unsupported;
        }
        caps
    }

    fn reasoning_efforts(&self, probe: &HarnessProbe) -> Vec<ReasoningEffort> {
        effort_ladder(probe).to_vec()
    }

    async fn list_models(&self, probe: &HarnessProbe) -> Vec<crate::ListedHarnessModel> {
        let Some(binary) = probe.binary_path.as_deref() else {
            return Vec::new();
        };
        with_model_reasoning_efforts(
            crate::prefer_gateway_models(
                crate::list_cli_models(binary, &["models"], &probe.env).await,
            ),
            probe.version.as_deref(),
            probe.reported_efforts.as_ref(),
        )
    }

    async fn launch(&self, spec: SessionSpec) -> Result<Box<dyn HarnessSession>, HarnessError> {
        let Some(binary) = spec.binary.as_deref().filter(|path| path.is_absolute()) else {
            return Err(HarnessError::NotFound);
        };
        let version = observe_version(binary, &spec.env)
            .await
            .unwrap_or_else(|_| "unknown".into());
        crate::grok::session::refuse_versioned_mode(spec.permission_mode, &version)?;
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
    command.env(DISABLE_AUTOUPDATER_ENV, "1");
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

    #[test]
    fn image_read_fixture_keeps_pixels_out_of_the_journal_preview() {
        let (events, unrecognized) = replay_version("1.0.13", "image-read");
        assert_eq!(unrecognized, 0);
        let preview = events.iter().find_map(|event| match event {
            HarnessEvent::ToolCompleted { preview, .. } => Some(preview.as_str()),
            _ => None,
        });
        assert_eq!(preview, Some("Image received (image/png)."));
        let rendered = serde_json::to_string(&events).unwrap();
        assert!(!rendered.contains("iVBOR"));
    }

    #[test]
    fn image_read_fixture_reaches_the_next_model_request_as_pixels() {
        let directory = fixture_dir("1.0.13");
        let stream = std::fs::read_to_string(directory.join("image-read.ndjson")).unwrap();
        let image_update = stream
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|value| value.pointer("/rawOutput/ImageContent/data").is_some())
            .unwrap();
        let request: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(directory.join("image-read-request.json")).unwrap(),
        )
        .unwrap();
        let pixels = image_update
            .pointer("/rawOutput/ImageContent/data")
            .and_then(serde_json::Value::as_str)
            .unwrap();
        let image = request["messages"][0]["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "image_url")
            .unwrap();
        assert_eq!(
            image["image_url"]["url"],
            format!("data:image/png;base64,{pixels}")
        );
        assert_eq!(request["messages"][0]["role"], "tool");
    }

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

    fn completed_usage(events: &[HarnessEvent]) -> tidebreak_core::TurnUsage {
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
    fn reasoning_efforts_follow_the_installed_patch_release() {
        assert_eq!(
            effort_ladder_for_version(Some("grok 1.0.4 (d846eb93d94d) [stable]")),
            EFFORT_LADDER_1_0_4
        );
        assert_eq!(
            effort_ladder_for_version(Some("grok 1.0.5 (5115b46bc909) [stable]")),
            EFFORT_LADDER_1_0_5
        );
        assert_eq!(
            effort_ladder_for_version(Some("grok 1.0.40 (eb1a2256660d) [stable]")),
            EFFORT_LADDER_CURRENT
        );
        assert_eq!(
            effort_ladder_for_version(Some("grok 1.1.0 [stable]")),
            EFFORT_LADDER_CURRENT
        );
    }

    /// The `initialize` answer the pinned release gave in its captured ACP
    /// session.
    fn pinned_initialize() -> serde_json::Value {
        let pin = crate::pin_for(HarnessKind::Grok).unwrap();
        let path = fixture_dir(pin.version).join("acp-plan.ndjson");
        let capture = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "capture an ACP session with its initialize answer from the {} pin into {}",
                pin.version,
                path.display()
            )
        });
        capture
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|frame| frame["direction"] == "server" && frame["value"]["id"] == 1)
            .map(|frame| frame["value"]["result"].clone())
            .expect("the capture holds the initialize answer")
    }

    /// Each model states its own ladder, and grok-4.5 stops at `high`. The
    /// listing takes the engine's word; the table would have offered
    /// `xhigh` for both.
    #[test]
    fn the_pinned_engine_states_each_models_ladder() {
        let reported = model_efforts_from_initialize(&pinned_initialize());
        assert_eq!(
            reported.get("grok-4.6").map(Vec::as_slice),
            Some(GROK_MODEL_EFFORTS)
        );
        assert_eq!(
            reported.get("grok-4.5").map(Vec::as_slice),
            Some(
                [
                    ReasoningEffort::Low,
                    ReasoningEffort::Medium,
                    ReasoningEffort::High
                ]
                .as_slice()
            )
        );
        let listed = |id: &str| crate::ListedHarnessModel {
            id: id.into(),
            label: id.into(),
            default: false,
            reasoning_efforts: Vec::new(),
            fast_mode: false,
        };
        let stamped = with_model_reasoning_efforts(
            vec![listed("grok-4.5"), listed("glm-5.3")],
            Some("grok 1.0.13 (5e9a58528b76)"),
            Some(&crate::ReportedEfforts {
                engine: Vec::new(),
                models: reported,
            }),
        );
        assert!(!stamped[0]
            .reasoning_efforts
            .contains(&ReasoningEffort::XHigh));
        assert_eq!(
            stamped[1].reasoning_efforts,
            effective_effort_ladder(Some("grok 1.0.13 (5e9a58528b76)"), Some("glm-5.3")),
            "a model the engine does not list keeps the table"
        );
    }

    /// The tables are the fallback for a probe that could not ask. For the
    /// pinned release they must still hold every level its models take: the
    /// `--reasoning-effort` vocabulary for any model, and the published Grok
    /// ladder for Grok models. A pin bump points this at the new release's
    /// capture.
    #[test]
    fn the_fallback_tables_cover_what_the_pinned_engine_reports() {
        let pin = crate::pin_for(HarnessKind::Grok).unwrap();
        let vocabulary = effort_ladder_for_version(Some(pin.version));
        for (model, ladder) in model_efforts_from_initialize(&pinned_initialize()) {
            assert!(!ladder.is_empty(), "{model}");
            for level in &ladder {
                assert!(vocabulary.contains(level), "{model} takes {level:?}");
                if model.starts_with("grok-") {
                    assert!(
                        GROK_MODEL_EFFORTS.contains(level),
                        "{model} takes {level:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_model_row_with_no_stated_ladder_is_left_to_the_tables() {
        let result = serde_json::json!({"_meta": {"modelState": {"availableModels": [
            {"modelId": "relay-row", "_meta": {"agentType": "grok-build-plan"}},
            {"modelId": "no-effort", "_meta": {"supportsReasoningEffort": false}},
            {"modelId": "odd-levels", "_meta": {"reasoningEfforts": [
                {"value": "minimal"}, {"value": "high"}, {"value": "low"}
            ]}}
        ]}}});
        let reported = model_efforts_from_initialize(&result);
        assert!(!reported.contains_key("relay-row"));
        assert_eq!(reported.get("no-effort"), Some(&Vec::new()));
        assert_eq!(
            reported.get("odd-levels"),
            Some(&vec![ReasoningEffort::Low, ReasoningEffort::High]),
            "a level Tidebreak cannot send is left out, and the rest ascend"
        );
    }

    #[test]
    fn grok_models_do_not_advertise_max() {
        let current = Some("grok 1.0.40 (eb1a2256660d) [stable]");
        for id in [
            "grok-4.7",
            "grok-4.6",
            "model-gateway-model-gateway/grok-4.7",
            "xai:grok-4.7",
        ] {
            let ladder = effective_effort_ladder(current, Some(id));
            assert_eq!(
                ladder,
                vec![
                    ReasoningEffort::Low,
                    ReasoningEffort::Medium,
                    ReasoningEffort::High,
                    ReasoningEffort::XHigh,
                ],
                "{id}"
            );
            assert!(!ladder.contains(&ReasoningEffort::Max), "{id}");
        }
        let glm = effective_effort_ladder(current, Some("model-gateway-model-gateway/glm-5.3"));
        assert!(glm.contains(&ReasoningEffort::Max));
        assert_eq!(
            clamp_effort(current, Some("grok-4.7"), ReasoningEffort::Max),
            Some(ReasoningEffort::XHigh)
        );
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
            reported_efforts: None,
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
    fn acp_capabilities_continue_on_the_captured_1_0_line() {
        let caps = GrokAdapter::new().capabilities(&HarnessProbe {
            found: true,
            binary_path: None,
            version: Some("grok 1.0.14".into()),
            authenticated: Some(true),
            stderr: String::new(),
            env: Vec::new(),
            commands: Vec::new(),
            reported_efforts: None,
        });
        assert_eq!(caps.structured_approvals, CapLevel::Supported);
        assert_eq!(caps.auto_mode, CapLevel::Supported);
        assert_eq!(caps.plan_mode, CapLevel::Unsupported);
    }

    #[test]
    fn auto_posture_degrades_off_the_1_0_line() {
        let caps = GrokAdapter::new().capabilities(&HarnessProbe {
            found: true,
            binary_path: None,
            version: Some("grok 1.1.0 (0badc0de) [stable]".into()),
            authenticated: Some(true),
            stderr: String::new(),
            env: Vec::new(),
            commands: Vec::new(),
            reported_efforts: None,
        });
        assert_eq!(caps.auto_mode, CapLevel::Unknown);
        // Off the captured version line, unverified approval and plan behavior
        // degrades to Unknown.
        assert_eq!(caps.structured_approvals, CapLevel::Unknown);
        assert_eq!(caps.mid_turn_steering, CapLevel::Unsupported);
        assert_eq!(caps.plan_mode, CapLevel::Unknown);
        assert_eq!(caps.allow_mode, CapLevel::Supported);
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
            new_session_id: None,
            prompt_file: Path::new("/tmp/prompt.txt"),
            mode: PermissionMode::Auto,
            model: None,
            effort: None,
            effort_ladder: EFFORT_LADDER_1_0_4,
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
            new_session_id: None,
            prompt_file: Path::new("/tmp/prompt.txt"),
            mode: PermissionMode::Allow,
            model: None,
            effort: None,
            effort_ladder: EFFORT_LADDER_1_0_4,
        })
        .unwrap();
        assert!(plan.argv.iter().any(|arg| arg == "--always-approve"));
    }

    /// On one machine the 1.0.13 pin ran 1.0.40: Grok's npm entrypoint runs
    /// the person's `~/.grok/bin/grok`, which Grok's own updater keeps
    /// current. The probe must run and report the release Tidebreak pinned.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_probe_runs_the_pinned_grok_when_the_persons_own_is_newer() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = crate::pin::fake_grok_install(tmp.path());
        let pin = crate::pin_for(HarnessKind::Grok).unwrap();
        let host = HostEnv {
            shell: tmp.path().join("missing-shell"),
            env: Vec::new(),
            clear_env: true,
            data_dir: Some(fake.data_dir.clone()),
            managed_node_root: Some(fake.node_root.clone()),
            harness_versions: Vec::new(),
            declared_binaries: Vec::new(),
            declared_env: Some(vec![
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("HOME".into(), fake.person_home.clone().into_os_string()),
            ]),
        };

        let probe = GrokAdapter::new().probe(&host).await;
        assert!(probe.found, "{}", probe.stderr);
        assert_eq!(
            probe.version.as_deref(),
            Some(format!("grok {} (shipped)", pin.version).as_str())
        );
        let binary = probe.binary_path.unwrap();
        assert!(
            binary.starts_with(crate::pin::install_dir(&fake.data_dir, pin)),
            "{}",
            binary.display()
        );
        assert!(
            !probe
                .env
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("GROK_HOME")),
            "sign-in, settings, and sessions stay in the person's own Grok home"
        );
    }

    /// Every launch turns Grok's update check off and leaves `GROK_HOME` to
    /// the person, where Grok keeps sign-in, settings, and sessions.
    #[test]
    fn every_launch_turns_off_self_update_and_keeps_the_persons_grok_home() {
        let plan = compose_print_plan(PrintLaunch {
            binary: Path::new("/data/tools/harnesses/grok/1.0.13/grok-home/bin/grok-1.0.13"),
            extra_argv: &[],
            cwd: Path::new("/workspace"),
            extra_env: &[(DISABLE_AUTOUPDATER_ENV.into(), "0".into())],
            relay_auth: None,
            relay_key_env: None,
            resume_ref: None,
            new_session_id: None,
            prompt_file: Path::new("/tmp/prompt.txt"),
            mode: PermissionMode::Auto,
            model: None,
            effort: None,
            effort_ladder: EFFORT_LADDER_1_0_4,
        })
        .unwrap();
        assert_eq!(
            plan.env
                .iter()
                .filter(|(name, _)| name == DISABLE_AUTOUPDATER_ENV)
                .map(|(_, value)| value.as_str())
                .collect::<Vec<_>>(),
            ["1"]
        );
        assert!(plan.env.iter().all(|(name, _)| name != "GROK_HOME"));
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
