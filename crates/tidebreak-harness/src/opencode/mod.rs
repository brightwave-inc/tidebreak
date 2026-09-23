//! opencode adapter. Tertiary tier.
//!
//! Process model for 1.18.18: one long-lived `opencode serve` child per
//! session, driven over HTTP + a directory-scoped SSE event stream. Chosen
//! over `opencode run --format json` because the installed version's serve
//! API is real — sessions, messages, `/event`, and a permission reply
//! surface — and the prompt stays off argv.
//!
//! OpenCode serve has no extra-read-root flag. `allowed_read_roots` are still
//! required to be absolute so a relative private path cannot slip through; the
//! engine then runs without additional read scoping rather than dropping the
//! field silently.

pub mod parse;
pub mod session;

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tidebreak_core::{CapLevel, HarnessCaps, HarnessKind};
use tokio::process::Command;
use tokio::time::timeout;

use crate::opencode::session::OpencodeSession;
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
                let version = match host.declared_version(HarnessKind::Opencode) {
                    Some(declared) => Some(declared.to_owned()),
                    None => observe_version(&capture.binary, &capture.env).await.ok(),
                };
                let authenticated = observe_auth(&capture.binary, &capture.env).await;
                HarnessProbe {
                    found: true,
                    binary_path: Some(capture.binary),
                    version,
                    authenticated,
                    stderr: capture.stderr,
                    env: capture.env,
                    commands: Vec::new(),
                    reported_efforts: None,
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
            // 1.18.18 `--help` lists no effort flag. Re-checked through
            // opencode-ai@1.18.23: top-level `--help` still has none, and
            // `run --variant` / prompt_async `variant` are provider-specific
            // model variants with no captured round-trip, so this stays
            // Unsupported.
            reasoning_levels: CapLevel::Unsupported,
            // session.diff was empty arrays; file.edited was not seen.
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
        // Off the captured 1.18 line the missing-effort finding no longer
        // applies: a later minor may grow an engine ladder, so the verdict
        // drops back to Unknown (decision 31 rule 3). 1.18.19–1.18.23 stay
        // on this line and stay Unsupported.
        if crate::probe::off_pinned_line(probe.version.as_deref(), (1, 18)) {
            caps.reasoning_levels = CapLevel::Unknown;
        }
        caps
    }

    /// The agent and permission ruleset ride `POST /session`, and resuming is
    /// `GET /session/{id}` — 1.18.18 captured no way to re-apply either to a
    /// session that already exists. A relaunch onto a stored resume ref would
    /// therefore keep the posture the session was created with while the
    /// record said otherwise, so the change is refused instead.
    fn relaunch_composes_permission_mode(&self) -> bool {
        false
    }

    async fn launch(&self, spec: SessionSpec) -> Result<Box<dyn HarnessSession>, HarnessError> {
        if !spec
            .binary
            .as_deref()
            .is_some_and(std::path::Path::is_absolute)
        {
            return Err(HarnessError::NotFound);
        }
        // The serve child spawns on the first turn, not here (decision 0064),
        // so attaching a session costs no engine runtime.
        Ok(Box::new(OpencodeSession::new(spec)))
    }
}

/// `opencode auth list`, read as a sign-in observation. Never reads tokens:
/// the listing names providers and variable names, not values.
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
    // The same variables the session child gets (`filter_engine_child_env`
    // passes opencode no provider keys), so a key the listing counts below is
    // one a session can also use.
    for (key, value) in crate::filter_child_env(env.iter().cloned()) {
        command.env(key, value);
    }
    let child = crate::spawn_process_tree(&mut command).ok()?;
    let output = timeout(AUTH_TIMEOUT, child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    AuthListing::parse(&String::from_utf8_lossy(&output.stdout)).signed_in()
}

/// What `opencode auth list` reports: the credentials opencode stored itself,
/// then the provider keys it found in its environment.
///
/// 1.18.27 prints two blocks, each closed by a count line:
///
/// ```text
/// └  0 credentials
/// └  1 environment variable
/// ```
///
/// The environment block appears only when a provider variable is set.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct AuthListing {
    /// The credentials count, when the listing printed one.
    credentials: Option<u64>,
    /// The environment-variable count. Zero when the block is absent.
    environment: u64,
}

impl AuthListing {
    fn parse(stdout: &str) -> Self {
        let mut listing = Self::default();
        for line in stdout.lines() {
            if let Some(count) = count_before(line, "credential") {
                listing.credentials = Some(count);
            } else if let Some(count) = count_before(line, "environment variable") {
                listing.environment = count;
            }
        }
        listing
    }

    /// The sign-in observation this listing supports.
    ///
    /// A stored credential or a provider key in the environment signs
    /// opencode in. An empty listing does not sign it out: opencode also
    /// runs providers its own config defines with no key at all, such as a
    /// local model server, and a key set in Tidebreak's engine settings
    /// reaches the session but not this listing. Neither shows up here, so
    /// an empty listing is unverified, which still lets a person pick the
    /// engine.
    fn signed_in(self) -> Option<bool> {
        match self.credentials {
            None => None,
            Some(credentials) if credentials > 0 || self.environment > 0 => Some(true),
            Some(_) => None,
        }
    }
}

/// The number a count line puts before `noun`: `└  3 credentials` → 3.
///
/// Only a line that is the count and nothing else matches, so a provider
/// named after the noun cannot read as a count.
fn count_before(line: &str, noun: &str) -> Option<u64> {
    let text = line.trim_start_matches(|ch: char| !ch.is_ascii_alphanumeric());
    let (number, rest) = text.split_once(' ')?;
    let rest = rest.trim_end();
    let plural = format!("{noun}s");
    if rest != noun && rest != plural {
        return None;
    }
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

    /// `opencode auth list` from the pinned 1.18.27, captured in a
    /// throwaway home. The version's manifest lists how each one was made.
    fn auth_list_capture(name: &str) -> String {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("fixtures/opencode/1.18.27/auth-list-{name}.txt"));
        std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("missing capture {}: {err}", path.display()))
    }

    #[test]
    fn a_stored_credential_or_an_environment_key_signs_opencode_in() {
        let stored = AuthListing::parse(&auth_list_capture("stored-key"));
        assert_eq!(
            stored,
            AuthListing {
                credentials: Some(1),
                environment: 0
            }
        );
        assert_eq!(stored.signed_in(), Some(true));
        // Nothing stored and ANTHROPIC_API_KEY set: this read as signed out
        // when only the credentials line counted.
        let environment = AuthListing::parse(&auth_list_capture("env-key"));
        assert_eq!(
            environment,
            AuthListing {
                credentials: Some(0),
                environment: 1
            }
        );
        assert_eq!(environment.signed_in(), Some(true));
        assert_eq!(
            AuthListing::parse(&auth_list_capture("stored-and-env")).signed_in(),
            Some(true)
        );
    }

    /// A home with nothing stored, and one whose only provider is a local
    /// model server in `opencode.json`, list the same thing. The second one
    /// works, so neither may read as signed out.
    #[test]
    fn an_empty_auth_list_is_unverified_rather_than_signed_out() {
        for name in ["none", "local-only"] {
            let listing = AuthListing::parse(&auth_list_capture(name));
            assert_eq!(
                listing,
                AuthListing {
                    credentials: Some(0),
                    environment: 0
                },
                "{name}"
            );
            assert_eq!(listing.signed_in(), None, "{name}");
        }
        // Output with no count line says nothing either way.
        assert_eq!(
            AuthListing::parse("error: could not open the database\n").signed_in(),
            None
        );
    }

    #[test]
    fn only_a_count_line_is_a_count() {
        assert_eq!(
            count_before("└  2 environment variables", "environment variable"),
            Some(2)
        );
        assert_eq!(
            count_before(
                "┌  Credentials \u{1b}[90m~/.local/share/opencode/auth.json",
                "credential"
            ),
            None
        );
        assert_eq!(
            count_before("●  Anthropic \u{1b}[90mapi", "credential"),
            None
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_reports_the_declared_version_without_asking_the_binary() {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("opencode");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o755)
            .open(&binary)
            .unwrap();
        // A binary that fails every invocation: only a skipped `--version`
        // can produce the declared version below.
        file.write_all(b"#!/bin/sh\nexit 1\n").unwrap();
        file.sync_all().unwrap();
        drop(file);
        let host = HostEnv {
            shell: dir.path().join("missing-shell"),
            env: Vec::new(),
            clear_env: true,
            data_dir: None,
            managed_node_root: None,
            harness_versions: Vec::new(),
            declared_binaries: Vec::new(),
            declared_env: None,
        }
        .with_declared_binary(tidebreak_core::HarnessKind::Opencode, &binary, "1.19.2");
        let probe = OpencodeAdapter::new().probe(&host).await;
        assert!(probe.found);
        assert_eq!(probe.binary_path.as_deref(), Some(binary.as_path()));
        assert_eq!(probe.version.as_deref(), Some("1.19.2"));
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
            reported_efforts: None,
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
    fn missing_effort_flag_finding_degrades_off_the_1_18_line() {
        let caps = OpencodeAdapter::new().capabilities(&HarnessProbe {
            found: true,
            binary_path: None,
            version: Some("1.19.2".into()),
            authenticated: Some(true),
            stderr: String::new(),
            env: Vec::new(),
            commands: Vec::new(),
            reported_efforts: None,
        });
        assert_eq!(caps.reasoning_levels, CapLevel::Unknown);
        // The serve API contract the adapter drives stays advertised.
        assert_eq!(caps.resume, CapLevel::Supported);
        assert_eq!(caps.structured_approvals, CapLevel::Supported);
        assert_eq!(caps.plan_mode, CapLevel::Supported);
        assert_eq!(caps.auto_mode, CapLevel::Supported);
        assert_eq!(caps.allow_mode, CapLevel::Supported);
    }

    #[test]
    fn launch_plan_never_includes_bypass_or_auto() {
        let plan = compose_serve_plan(crate::opencode::session::ServeLaunch {
            binary: Path::new("/usr/bin/opencode"),
            extra_argv: &[],
            cwd: Path::new("/workspace"),
            snapshot_env: &[],
            extra_env: &[],
            port: 4096,
            browser: None,
            native: None,
            apps: None,
            relay_key_env: None,
        })
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
        assert_eq!(count_before("└  3 credentials", "credential"), Some(3));
        assert_eq!(count_before("└  0 credentials", "credential"), Some(0));
    }
}
