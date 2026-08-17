//! Code-mode wire types and the HTTP+WebSocket client methods that speak them.
//!
//! These types mirror the renderer-facing snapshots in
//! `tidebreak-server::routes::code::types`. They are decoded loosely on purpose:
//! a field the CLI does not render is dropped, and an unrecognized event kind
//! still advances the journal cursor.

use serde::{Deserialize, Serialize};
use tidebreak_core::{
    AgentError, Attention, CapLevel, CodeApprovalId, CodeApprovalKind, CodeApprovalState,
    CodeEvent, CodePermissionMode, CodeSessionId, CodeSessionLifecycle, CodeTurnId, CodeTurnStatus,
    CodeUsage, CodeWorkspaceStatus, Diffstat, FenceReason, FileChangeKind, HarnessCaps,
    HarnessKind, HarnessTier, PullRequestDigest, QuickAction, RepoId, Result, WorkspaceId,
};

use super::client::{Client, EventSocket};

/// A registered local git repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeRepoSnapshot {
    pub id: RepoId,
    pub root_path: String,
    pub display_name: String,
    pub default_base_ref: String,
    pub branch_prefix: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_script: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_script: Option<String>,
    #[serde(default)]
    pub quick_actions: Vec<QuickAction>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// One isolated workspace (worktree + branch) on a repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeWorkspaceSnapshot {
    pub id: WorkspaceId,
    pub repo_id: RepoId,
    pub title: String,
    pub worktree_path: String,
    pub branch_name: String,
    pub base_ref: String,
    pub status: CodeWorkspaceStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<PullRequestDigest>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// One durable conversation with an external agent engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeSessionSnapshot {
    pub id: CodeSessionId,
    pub workspace_id: WorkspaceId,
    pub harness_kind: HarnessKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_resume_ref: Option<String>,
    pub permission_mode: CodePermissionMode,
    pub lifecycle: CodeSessionLifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fence_reason: Option<FenceReason>,
    pub attention: Attention,
    #[serde(default)]
    pub unrecognized_event_count: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// One user→engine turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeTurnSnapshot {
    pub id: CodeTurnId,
    pub session_id: CodeSessionId,
    pub ordinal: i64,
    pub status: CodeTurnStatus,
    pub user_input: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<CodeUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diffstat: Option<Diffstat>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// A follow-up parked while the session is already running a turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedCodeTurn {
    pub session_id: CodeSessionId,
    pub message: String,
    pub position: i64,
}

/// Result of `POST /code/sessions/{id}/turns`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SubmitTurnResponse {
    Ran(CodeTurnSnapshot),
    Queued(QueuedCodeTurn),
}

/// One journaled event on the per-session WebSocket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SequencedCodeEventFrame {
    pub seq: i64,
    pub event: CodeEvent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replayed: Option<bool>,
}

/// Doctor report for every registered engine adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessDoctorReport {
    pub harnesses: Vec<HarnessDoctorEntry>,
}

/// One engine's probe, capabilities, and remediation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessDoctorEntry {
    pub kind: HarnessKind,
    pub found: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub tier: HarnessTier,
    pub caps: HarnessCaps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authenticated: Option<bool>,
    #[serde(default)]
    pub remediation: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub unrecognized_event_count: i64,
}

/// Bounded changed-file list for `GET /code/workspaces/{id}/files`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeWorkspaceFiles {
    pub files: Vec<CodeFileChange>,
    pub truncated: bool,
    pub stat: Diffstat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<CodeTurnId>,
}

/// One changed path in a workspace or turn file list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeFileChange {
    pub path: String,
    pub kind: FileChangeKind,
    pub insertions: u32,
    pub deletions: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_path: Option<String>,
}

/// Bounded unified diff for `GET /code/workspaces/{id}/diff`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeWorkspaceDiff {
    pub diff: String,
    pub truncated: bool,
    pub stat: Diffstat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<CodeTurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

/// One parked or decided engine approval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeApprovalSnapshot {
    pub id: CodeApprovalId,
    pub session_id: CodeSessionId,
    pub turn_id: CodeTurnId,
    pub kind: CodeApprovalKind,
    pub harness_raw_json: String,
    pub state: CodeApprovalState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    pub requested_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Result of staging and committing the workspace worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeCommitSnapshot {
    pub sha: String,
    pub message: String,
    pub stat: Diffstat,
}

/// Result of pushing the workspace branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodePushSnapshot {
    pub branch: String,
    pub remote: String,
}

/// PR + checks digest plus the local git facts the PR card needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeWorkspacePrSnapshot {
    pub dirty: bool,
    pub unpushed: bool,
    pub ahead: u64,
    pub has_upstream: bool,
    pub suggested_commit_message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<PullRequestDigest>,
    pub gh_found: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gh_authenticated: Option<bool>,
    #[serde(default)]
    pub remediation: String,
}

/// Bounded output of one named quick action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeActionSnapshot {
    pub name: String,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Cheap per-session digest on `/code/updates`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeSessionDigest {
    pub workspace: WorkspaceId,
    pub session: CodeSessionId,
    pub lifecycle: CodeSessionLifecycle,
    pub attention: Attention,
    pub title: String,
    pub turn_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_state: Option<PullRequestDigest>,
}

/// One unsequenced notice on `WS /code/updates`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CodeUpdateNotice {
    Snapshot {
        sessions: Vec<CodeSessionDigest>,
    },
    Digest {
        workspace: WorkspaceId,
        session: CodeSessionId,
        lifecycle: CodeSessionLifecycle,
        attention: Attention,
        title: String,
        turn_count: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pr_state: Option<PullRequestDigest>,
    },
    TerminalActivity {
        workspace_id: WorkspaceId,
        terminal_id: String,
    },
    #[serde(other)]
    Unknown,
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
        permission_mode: CodePermissionMode,
    ) -> Result<CodeSessionSnapshot> {
        self.post_json(
            format!("{}/code/workspaces/{workspace}/sessions", self.base_url()),
            &serde_json::json!({
                "harness": harness,
                "permission_mode": permission_mode,
            }),
        )
        .await
    }

    pub async fn submit_turn(
        &self,
        session: CodeSessionId,
        message: &str,
    ) -> Result<SubmitTurnResponse> {
        self.post_json(
            format!("{}/code/sessions/{session}/turns", self.base_url()),
            &serde_json::json!({ "message": message }),
        )
        .await
    }

    pub async fn list_session_turns(
        &self,
        session: CodeSessionId,
    ) -> Result<Vec<CodeTurnSnapshot>> {
        self.get_json(format!("{}/code/sessions/{session}/turns", self.base_url()))
            .await
    }

    pub async fn interrupt_session(&self, session: CodeSessionId) -> Result<()> {
        self.post_ok(
            format!("{}/code/sessions/{session}/interrupt", self.base_url()),
            &serde_json::json!({}),
        )
        .await
    }

    pub async fn reap_session(&self, session: CodeSessionId) -> Result<CodeSessionSnapshot> {
        self.post_json(
            format!("{}/code/sessions/{session}/reap", self.base_url()),
            &serde_json::json!({}),
        )
        .await
    }

    pub async fn list_approvals(
        &self,
        session: Option<CodeSessionId>,
        pending_only: bool,
    ) -> Result<Vec<CodeApprovalSnapshot>> {
        let mut url = format!("{}/code/approvals", self.base_url());
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
            format!("{}/code/approvals/{id}/decision", self.base_url()),
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
        self.open_ws(&format!("/code/sessions/{session}/events?after={after}"))
            .await
    }

    pub async fn open_code_updates(&self) -> Result<EventSocket> {
        self.open_ws("/code/updates").await
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
/// everything else is a failure of the work, not of the command itself.
pub fn turn_exit_code(event: &CodeEvent) -> Option<i32> {
    match event {
        CodeEvent::TurnCompleted { .. } => Some(0),
        CodeEvent::TurnFailed { .. } | CodeEvent::TurnInterrupted => Some(1),
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

/// Decode one `/code/updates` notice. An unrecognized tag becomes
/// [`CodeUpdateNotice::Unknown`] rather than failing the stream.
pub fn decode_update_notice(text: &str) -> Result<CodeUpdateNotice> {
    serde_json::from_str(text)
        .map_err(|error| AgentError::msg(format!("bad code update notice: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::{AttentionSource, AttentionState};

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

    #[test]
    fn an_unknown_update_tag_does_not_kill_the_stream() {
        let notice = decode_update_notice(r#"{"type":"future_kind","extra":true}"#).unwrap();
        assert!(matches!(notice, CodeUpdateNotice::Unknown));
    }
}
