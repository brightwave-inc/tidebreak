//! Code mode as the CLI drives it: the server's wire types, and the HTTP and
//! WebSocket client methods that speak them.
//!
//! The snapshot, frame, and notice types are the server's own, re-exported
//! from [`tidebreak_server::wire`]. The CLI used to keep a hand-written mirror
//! here, and it drifted: a field the server renamed or made optional compiled
//! on both sides and failed when the CLI next read it. Importing the server's
//! types makes a rename a compile error, and brings the renderer's strictness
//! with it (see the `wire` module doc): unknown keys fail a snapshot, an
//! unknown notice tag fails the notice, and an unknown event type fails its
//! frame. A reader drops a failed frame or notice and keeps the socket open.
//!
//! One tolerance stays, and it is the server's, not this crate's: the event
//! union inside a frame is `tidebreak_core::CodeEvent`, which the server also
//! reads back from its own journal, so a variant accepts keys it does not
//! declare. The frame around it does not.
//!
//! `crates/tidebreak-server/fixtures/code-frames.json` holds one real value of
//! every snapshot, notice, and event; the test at the bottom decodes each one.

use serde::{Deserialize, Serialize};
use tidebreak_core::{
    AgentError, CapLevel, CodeApprovalId, CodeEvent, CodeSessionId, CodeTurnId, HarnessCaps,
    HarnessKind, PermissionMode, RepoId, Result, WorkspaceId,
};

pub use tidebreak_server::wire::{
    CodeActionSnapshot, CodeApprovalSnapshot, CodeCommitSnapshot, CodePushSnapshot,
    CodeRepoSnapshot, CodeSessionDigest, CodeSessionSnapshot, CodeTurnSnapshot, CodeUpdateNotice,
    CodeWorkspaceDiff, CodeWorkspaceFiles, CodeWorkspacePrSnapshot, CodeWorkspaceSnapshot,
    HarnessAuthMode, HarnessDoctorReport, QueuedCodeTurn, QueuedCodeTurnsSnapshot,
    SequencedCodeEventFrame,
};

use super::client::{Client, EventSocket};

/// Result of `POST /sessions/{id}/turns`: the turn that ran on `200`, or
/// the follow-up the server parked on `202`. The server answers with one of
/// two snapshots rather than a tagged union, so this is the one code-mode
/// shape the client composes itself. Both arms reject unknown keys, so a
/// snapshot that matches neither fails rather than folding into the other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SubmitTurnResponse {
    Ran(CodeTurnSnapshot),
    Queued(QueuedCodeTurn),
}

impl Client {
    pub async fn list_harnesses(&self) -> Result<HarnessDoctorReport> {
        self.get_json(format!("{}/code/harnesses", self.base_url()))
            .await
    }

    pub async fn refresh_harnesses(&self) -> Result<HarnessDoctorReport> {
        self.post_json(
            format!("{}/code/harnesses/refresh", self.base_url()),
            &serde_json::json!({}),
        )
        .await
    }

    pub async fn create_repo(
        &self,
        path: &str,
        display_name: Option<&str>,
        default_base_ref: Option<&str>,
        branch_prefix: Option<&str>,
    ) -> Result<CodeRepoSnapshot> {
        let mut body = serde_json::json!({ "path": path });
        if let Some(name) = display_name {
            body["display_name"] = name.into();
        }
        if let Some(base) = default_base_ref {
            body["default_base_ref"] = base.into();
        }
        if let Some(prefix) = branch_prefix {
            body["branch_prefix"] = prefix.into();
        }
        self.post_json(format!("{}/code/repos", self.base_url()), &body)
            .await
    }

    pub async fn list_repos(&self) -> Result<Vec<CodeRepoSnapshot>> {
        self.get_json(format!("{}/code/repos", self.base_url()))
            .await
    }

    pub async fn delete_repo(&self, id: RepoId) -> Result<()> {
        self.delete_ok(format!("{}/code/repos/{id}", self.base_url()))
            .await
    }

    pub async fn create_workspace(
        &self,
        repo_id: RepoId,
        title: Option<&str>,
        base_ref: Option<&str>,
    ) -> Result<CodeWorkspaceSnapshot> {
        let mut body = serde_json::json!({ "repo_id": repo_id });
        if let Some(title) = title {
            body["title"] = title.into();
        }
        if let Some(base) = base_ref {
            body["base_ref"] = base.into();
        }
        self.post_json(format!("{}/code/workspaces", self.base_url()), &body)
            .await
    }

    pub async fn list_workspaces(
        &self,
        repo_id: Option<RepoId>,
    ) -> Result<Vec<CodeWorkspaceSnapshot>> {
        let mut url = format!("{}/code/workspaces", self.base_url());
        if let Some(repo_id) = repo_id {
            url.push_str(&format!("?repo_id={repo_id}"));
        }
        self.get_json(url).await
    }

    pub async fn get_workspace(&self, id: WorkspaceId) -> Result<CodeWorkspaceSnapshot> {
        self.get_json(format!("{}/code/workspaces/{id}", self.base_url()))
            .await
    }

    pub async fn archive_workspace(
        &self,
        id: WorkspaceId,
        force: bool,
    ) -> Result<CodeWorkspaceSnapshot> {
        self.post_json(
            format!("{}/code/workspaces/{id}/archive", self.base_url()),
            &serde_json::json!({ "force": force }),
        )
        .await
    }

    pub async fn list_workspace_sessions(
        &self,
        id: WorkspaceId,
    ) -> Result<Vec<CodeSessionSnapshot>> {
        self.get_json(format!("{}/code/workspaces/{id}/sessions", self.base_url()))
            .await
    }

    pub async fn create_session(
        &self,
        workspace: WorkspaceId,
        harness: HarnessKind,
        permission_mode: PermissionMode,
    ) -> Result<CodeSessionSnapshot> {
        self.create_session_with(workspace, harness, permission_mode, None)
            .await
    }

    pub async fn create_session_with(
        &self,
        workspace: WorkspaceId,
        harness: HarnessKind,
        permission_mode: PermissionMode,
        model: Option<&str>,
    ) -> Result<CodeSessionSnapshot> {
        let mut body = serde_json::json!({
            "harness": harness,
            "permission_mode": permission_mode,
        });
        if let Some(model) = model {
            body["model"] = model.into();
        }
        self.post_json(
            format!("{}/code/workspaces/{workspace}/sessions", self.base_url()),
            &body,
        )
        .await
    }

    pub async fn submit_turn(
        &self,
        session: CodeSessionId,
        message: &str,
    ) -> Result<SubmitTurnResponse> {
        self.post_json(
            format!("{}/sessions/{session}/turns", self.base_url()),
            &serde_json::json!({ "message": message }),
        )
        .await
    }

    pub async fn list_session_turns(
        &self,
        session: CodeSessionId,
    ) -> Result<Vec<CodeTurnSnapshot>> {
        self.get_json(format!("{}/sessions/{session}/turns", self.base_url()))
            .await
    }

    pub async fn list_queued_code_turns(
        &self,
        session: CodeSessionId,
    ) -> Result<QueuedCodeTurnsSnapshot> {
        self.get_json(format!("{}/sessions/{session}/queued", self.base_url()))
            .await
    }

    pub async fn interrupt_session(&self, session: CodeSessionId) -> Result<()> {
        self.post_ok(
            format!("{}/sessions/{session}/interrupt", self.base_url()),
            &serde_json::json!({}),
        )
        .await
    }

    pub async fn reap_session(&self, session: CodeSessionId) -> Result<CodeSessionSnapshot> {
        self.post_json(
            format!("{}/sessions/{session}/reap", self.base_url()),
            &serde_json::json!({}),
        )
        .await
    }

    pub async fn set_session_permission_mode(
        &self,
        session: CodeSessionId,
        mode: PermissionMode,
    ) -> Result<CodeSessionSnapshot> {
        self.post_json(
            format!("{}/sessions/{session}/mode", self.base_url()),
            &serde_json::json!({ "permission_mode": mode }),
        )
        .await
    }

    pub async fn list_approvals(
        &self,
        session: Option<CodeSessionId>,
        pending_only: bool,
    ) -> Result<Vec<CodeApprovalSnapshot>> {
        let mut url = format!("{}/approvals", self.base_url());
        let mut separator = '?';
        if pending_only {
            url.push_str("?state=pending");
            separator = '&';
        }
        if let Some(session) = session {
            url.push(separator);
            url.push_str("session_id=");
            url.push_str(&session.to_string());
        }
        self.get_json(url).await
    }

    pub async fn decide_code_approval(
        &self,
        id: CodeApprovalId,
        approve: bool,
        feedback: Option<&str>,
    ) -> Result<CodeApprovalSnapshot> {
        let mut body = serde_json::json!({
            "decision": if approve { "approve" } else { "deny" },
        });
        if let Some(feedback) = feedback {
            body["feedback"] = feedback.into();
        }
        self.post_json(
            format!("{}/approvals/{id}/decision", self.base_url()),
            &body,
        )
        .await
    }

    pub async fn workspace_files(
        &self,
        workspace: WorkspaceId,
        turn: Option<CodeTurnId>,
    ) -> Result<CodeWorkspaceFiles> {
        let mut url = format!("{}/code/workspaces/{workspace}/files", self.base_url());
        if let Some(turn) = turn {
            url.push_str(&format!("?turn={turn}"));
        }
        self.get_json(url).await
    }

    pub async fn workspace_diff(
        &self,
        workspace: WorkspaceId,
        turn: Option<CodeTurnId>,
        file: Option<&str>,
    ) -> Result<CodeWorkspaceDiff> {
        let mut url = format!("{}/code/workspaces/{workspace}/diff", self.base_url());
        let mut separator = '?';
        if let Some(turn) = turn {
            url.push(separator);
            url.push_str("turn=");
            url.push_str(&turn.to_string());
            separator = '&';
        }
        if let Some(file) = file {
            url.push(separator);
            url.push_str("file=");
            url.push_str(&urlencode(file));
        }
        self.get_json(url).await
    }

    pub async fn git_commit(
        &self,
        workspace: WorkspaceId,
        message: Option<&str>,
    ) -> Result<CodeCommitSnapshot> {
        let body = match message {
            Some(message) => serde_json::json!({ "message": message }),
            None => serde_json::json!({}),
        };
        self.post_json(
            format!("{}/code/workspaces/{workspace}/git/commit", self.base_url()),
            &body,
        )
        .await
    }

    pub async fn git_push(&self, workspace: WorkspaceId) -> Result<CodePushSnapshot> {
        self.post_json(
            format!("{}/code/workspaces/{workspace}/git/push", self.base_url()),
            &serde_json::json!({}),
        )
        .await
    }

    pub async fn git_pr(
        &self,
        workspace: WorkspaceId,
        title: Option<&str>,
        body: Option<&str>,
    ) -> Result<CodeWorkspacePrSnapshot> {
        let mut payload = serde_json::json!({});
        if let Some(title) = title {
            payload["title"] = title.into();
        }
        if let Some(body) = body {
            payload["body"] = body.into();
        }
        self.post_json(
            format!("{}/code/workspaces/{workspace}/git/pr", self.base_url()),
            &payload,
        )
        .await
    }

    pub async fn git_status(&self, workspace: WorkspaceId) -> Result<CodeWorkspacePrSnapshot> {
        self.get_json(format!(
            "{}/code/workspaces/{workspace}/pr",
            self.base_url()
        ))
        .await
    }

    pub async fn run_action(
        &self,
        workspace: WorkspaceId,
        name: &str,
    ) -> Result<CodeActionSnapshot> {
        self.post_json(
            format!(
                "{}/code/workspaces/{workspace}/actions/{name}",
                self.base_url()
            ),
            &serde_json::json!({}),
        )
        .await
    }

    pub async fn open_code_events(
        &self,
        session: CodeSessionId,
        after: i64,
    ) -> Result<EventSocket> {
        self.open_ws(&format!("/sessions/{session}/events?after={after}"))
            .await
    }

    pub async fn open_code_updates(&self) -> Result<EventSocket> {
        self.open_ws("/updates").await
    }
}

/// Summarize capability flags that are actually supported, for the doctor table.
pub fn supported_caps_summary(caps: &HarnessCaps) -> String {
    let flags = [
        ("resume", caps.resume),
        ("streaming", caps.streaming_deltas),
        ("approvals", caps.structured_approvals),
        ("steering", caps.mid_turn_steering),
        ("plan", caps.plan_mode),
        ("auto", caps.auto_mode),
        ("allow", caps.allow_mode),
        ("reasoning", caps.reasoning_levels),
        ("file_events", caps.native_file_change_events),
        ("interrupt", caps.native_interrupt),
    ];
    let supported: Vec<&str> = flags
        .into_iter()
        .filter(|(_, level)| *level == CapLevel::Supported)
        .map(|(name, _)| name)
        .collect();
    if supported.is_empty() {
        "none".to_owned()
    } else {
        supported.join(",")
    }
}

fn urlencode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// Exit status a `tidebreak code run` turn maps to. Completed is zero;
/// everything else — a failure, an interruption, a model refusal — is a
/// failure of the work, not of the command itself, as `tidebreak chat`
/// already reads them.
pub fn turn_exit_code(event: &CodeEvent) -> Option<i32> {
    match event {
        CodeEvent::TurnCompleted { .. } => Some(0),
        CodeEvent::TurnFailed { .. }
        | CodeEvent::TurnInterrupted { .. }
        | CodeEvent::TurnRefused { .. } => Some(1),
        _ => None,
    }
}

/// Whether this event ends the turn `code run` is waiting on.
pub fn is_turn_terminal(event: &CodeEvent) -> bool {
    turn_exit_code(event).is_some()
}

/// Decode one session-event socket frame. Unknown payloads are ignored by
/// the human renderer; `--json` prints the raw line instead of this type.
pub fn decode_event_frame(text: &str) -> Result<SequencedCodeEventFrame> {
    serde_json::from_str(text)
        .map_err(|error| AgentError::msg(format!("bad code event frame: {error}")))
}

/// Decode one `/updates` notice. An unrecognized tag becomes
/// [`CodeUpdateNotice::Unknown`] rather than failing the stream.
pub fn decode_update_notice(text: &str) -> Result<CodeUpdateNotice> {
    serde_json::from_str(text)
        .map_err(|error| AgentError::msg(format!("bad code update notice: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::{AttentionSource, AttentionState, CodeSessionLifecycle};

    #[test]
    fn a_sequenced_frame_round_trips_the_fields_agents_script_against() {
        let raw = serde_json::json!({
            "seq": 4,
            "event": {
                "type": "assistant_delta",
                "text": "hello"
            },
            "replayed": true
        });
        let frame: SequencedCodeEventFrame = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(frame.seq, 4);
        assert_eq!(frame.replayed, Some(true));
        assert!(matches!(
            frame.event,
            CodeEvent::AssistantDelta { ref text } if text == "hello"
        ));
        let encoded = serde_json::to_value(&frame).unwrap();
        assert_eq!(encoded["seq"], 4);
        assert_eq!(encoded["event"]["type"], "assistant_delta");
        assert_eq!(encoded["event"]["text"], "hello");
        assert_eq!(encoded["replayed"], true);
    }

    #[test]
    fn a_transient_delta_frame_keeps_the_cursor_it_streamed_behind() {
        // Deltas are live-only (record 57). `code run --json` prints them the
        // same as any other frame, and the `seq` they carry is the journal
        // position to resume from. A reconnect receives the complete current
        // tail as a replacement frame.
        let frame = decode_event_frame(
            r#"{"seq":12,"event":{"type":"assistant_delta","text":"half"},"transient":true,"replacement":true}"#,
        )
        .unwrap();
        assert_eq!(frame.seq, 12);
        assert_eq!(frame.transient, Some(true));
        assert_eq!(frame.replacement, Some(true));
        assert_eq!(frame.replayed, None);
        assert!(matches!(
            frame.event,
            CodeEvent::AssistantDelta { ref text } if text == "half"
        ));
    }

    #[test]
    fn a_turn_completed_frame_is_the_zero_exit() {
        let frame = decode_event_frame(
            r#"{"seq":9,"event":{"type":"turn_completed","usage":{"input_tokens":1,"output_tokens":2}}}"#,
        )
        .unwrap();
        assert_eq!(turn_exit_code(&frame.event), Some(0));
        assert!(is_turn_terminal(&frame.event));
    }

    #[test]
    fn failed_and_interrupted_turns_are_nonzero() {
        let failed = decode_event_frame(
            r#"{"seq":3,"event":{"type":"turn_failed","error":{"message":"boom"}}}"#,
        )
        .unwrap();
        let interrupted =
            decode_event_frame(r#"{"seq":4,"event":{"type":"turn_interrupted"}}"#).unwrap();
        assert_eq!(turn_exit_code(&failed.event), Some(1));
        assert_eq!(turn_exit_code(&interrupted.event), Some(1));
    }

    #[test]
    fn update_snapshot_and_digest_keep_their_tags() {
        let snapshot = decode_update_notice(r#"{"type":"snapshot","sessions":[]}"#).unwrap();
        assert!(matches!(snapshot, CodeUpdateNotice::Snapshot { sessions } if sessions.is_empty()));

        let digest = decode_update_notice(
            &serde_json::json!({
                "type": "digest",
                "workspace": "00000000-0000-0000-0000-000000000001",
                "session": "00000000-0000-0000-0000-000000000002",
                "kind": "interactive",
                "lifecycle": "running",
                "attention": {
                    "state": { "type": "working" },
                    "source": "lifecycle"
                },
                "title": "fix login",
                "turn_count": 2
            })
            .to_string(),
        )
        .unwrap();
        match digest {
            CodeUpdateNotice::Digest {
                title,
                turn_count,
                attention,
                lifecycle,
                ..
            } => {
                assert_eq!(title, "fix login");
                assert_eq!(turn_count, 2);
                assert_eq!(lifecycle, CodeSessionLifecycle::Running);
                assert!(matches!(attention.state, AttentionState::Working));
                assert_eq!(attention.source, AttentionSource::Lifecycle);
            }
            other => panic!("expected digest, got {other:?}"),
        }
    }

    /// An unknown notice tag fails the notice; the watch loop drops it and
    /// keeps reading. The old mirror folded it to an `Unknown` variant, which
    /// is the same outcome with a shape the server never declared.
    #[test]
    fn an_unknown_update_tag_fails_the_notice() {
        assert!(decode_update_notice(r#"{"type":"future_kind","extra":true}"#).is_err());
    }

    /// A key the server does not declare fails the snapshot, as it does in
    /// the renderer.
    #[test]
    fn snapshots_reject_unknown_keys() {
        let queued = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000006",
            "session_id": "00000000-0000-0000-0000-000000000003",
            "message": "then the tests",
            "position": 0,
            "created_at": "2026-09-01T04:56:10Z",
            "updated_at": "2026-09-01T04:56:10Z",
        });
        assert!(serde_json::from_value::<SubmitTurnResponse>(queued.clone()).is_ok());
        let mut extra = queued;
        extra["extra"] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<SubmitTurnResponse>(extra).is_err());
    }

    /// Path of the server's code-mode fixtures, relative to this crate.
    const CODE_FRAMES: &str = "../tidebreak-server/fixtures/code-frames.json";

    fn code_frame_fixtures() -> Vec<(String, String, serde_json::Value)> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(CODE_FRAMES);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let entries: Vec<serde_json::Value> =
            serde_json::from_str(&text).expect("the fixture file is a JSON array");
        entries
            .into_iter()
            .map(|entry| {
                let name = entry["name"].as_str().expect("every fixture is named");
                let kind = entry["kind"].as_str().expect("every fixture has a kind");
                (name.to_owned(), kind.to_owned(), entry["value"].clone())
            })
            .collect()
    }

    fn round_trip<T: serde::de::DeserializeOwned + Serialize>(
        name: &str,
        value: &serde_json::Value,
    ) {
        let decoded: T = serde_json::from_value(value.clone())
            .unwrap_or_else(|error| panic!("fixture {name} does not decode: {error}"));
        let again = serde_json::to_value(&decoded).expect("a decoded value serializes");
        assert_eq!(
            &again, value,
            "fixture {name} changed across the round trip"
        );
    }

    /// Every snapshot, notice, and event the server serializes decodes here,
    /// byte for byte. The fixtures come from the server's own types, and the
    /// renderer's tests read the same file, so the three decoders cannot
    /// drift apart without one of them failing.
    #[test]
    fn every_server_code_frame_decodes() {
        let fixtures = code_frame_fixtures();
        assert!(fixtures.len() > 40, "the fixture list looks truncated");
        for (name, kind, value) in &fixtures {
            match kind.as_str() {
                "repo" => round_trip::<CodeRepoSnapshot>(name, value),
                "workspace" => round_trip::<CodeWorkspaceSnapshot>(name, value),
                "session" => round_trip::<CodeSessionSnapshot>(name, value),
                "turn" => round_trip::<SubmitTurnResponse>(name, value),
                "queued_turn" => round_trip::<SubmitTurnResponse>(name, value),
                "queued_turns" => round_trip::<QueuedCodeTurnsSnapshot>(name, value),
                "harness_doctor" => round_trip::<HarnessDoctorReport>(name, value),
                "workspace_files" => round_trip::<CodeWorkspaceFiles>(name, value),
                "workspace_diff" => round_trip::<CodeWorkspaceDiff>(name, value),
                "approval" => round_trip::<CodeApprovalSnapshot>(name, value),
                "commit" => round_trip::<CodeCommitSnapshot>(name, value),
                "push" => round_trip::<CodePushSnapshot>(name, value),
                "workspace_pr" => round_trip::<CodeWorkspacePrSnapshot>(name, value),
                "action" => round_trip::<CodeActionSnapshot>(name, value),
                "session_digest" => round_trip::<CodeSessionDigest>(name, value),
                "update_notice" => {
                    let notice = decode_update_notice(&value.to_string())
                        .unwrap_or_else(|error| panic!("fixture {name}: {error}"));
                    assert_eq!(&serde_json::to_value(&notice).unwrap(), value, "{name}");
                }
                "event_frame" => {
                    let frame = decode_event_frame(&value.to_string())
                        .unwrap_or_else(|error| panic!("fixture {name}: {error}"));
                    assert_eq!(&serde_json::to_value(&frame).unwrap(), value, "{name}");
                    assert_eq!(
                        turn_exit_code(&frame.event).is_some(),
                        is_turn_terminal(&frame.event)
                    );
                }
                other => panic!("fixture {name} has no decoder for kind {other}"),
            }
        }
    }

    /// The submit response tells the two server answers apart by shape.
    #[test]
    fn submit_response_arms_follow_the_fixtures() {
        let mut ran = 0;
        let mut queued = 0;
        for (name, kind, value) in code_frame_fixtures() {
            match kind.as_str() {
                "turn" => {
                    assert!(
                        matches!(
                            serde_json::from_value(value).unwrap(),
                            SubmitTurnResponse::Ran(_)
                        ),
                        "{name}"
                    );
                    ran += 1;
                }
                "queued_turn" => {
                    assert!(
                        matches!(
                            serde_json::from_value(value).unwrap(),
                            SubmitTurnResponse::Queued(_)
                        ),
                        "{name}"
                    );
                    queued += 1;
                }
                _ => {}
            }
        }
        assert!(ran > 0 && queued > 0);
    }
}
