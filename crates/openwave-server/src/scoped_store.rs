//! The request-facing store view, bound to one authenticated principal.
//!
//! Route handlers do not touch [`Store`] directly. They extract a
//! [`ScopedStore`] — the durable store bound to the requesting principal's
//! [`OwnerId`] — and every root-aggregate query on it (chats, projects,
//! documents) is the trait's owner-scoped variant. The unscoped root surface
//! simply does not exist on this type, and the inner handle never escapes it,
//! so route code cannot express a query that crosses owners (#853).
//!
//! Non-root operations (turns, agent runs, events, approvals) hang off a
//! root by id and pass through unchanged; the handler authorizes the root
//! through this view first, exactly as the store's scoping model prescribes.
//! Standing grants, which span chats, are owner-scoped directly through the
//! chat or project their level points at. System paths that act on
//! already-authorized ids — turn workers, retirement scans, post-commit
//! signalling — are not requests and keep the separate unscoped handle on
//! [`AppState`].
//!
//! The extractor fails closed like [`AuthContext`] itself: on a route the
//! auth middleware does not cover it answers `401`, never a defaulted owner.

use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use serde_json::Value;

use openwave_core::storage::DecidePlanOutcome;
use openwave_core::{
    AcceptTurnOutcome, AcceptTurnSteerOutcome, AgentRun, AgentRunId, AgentRunResult,
    AnswerUserQuestionsOutcome, AnswerUserQuestionsRequest, CallId, Chat, ChatId,
    ChatTranscriptSnapshot, ClaimClientToolCallOutcome, DecidePlanRequest, DeleteChatOutcome,
    DeleteProjectOutcome, DocumentId, DocumentListCursor, DocumentRecord, DocumentScope,
    DocumentSourceUpsert, DocumentSummaryRecord, HeartbeatClientToolCallOutcome, ImageRef,
    JournaledClientToolCallOutcome, JournaledTurnOutcome, MessageAttachment, MoveChatOutcome,
    NetworkPolicy, OwnerId, PendingPlanApproval, PendingUserQuestions, PermissionMode, Project,
    ProjectId, ReasoningEffort, RequestAgentRunCancellationOutcome, RequestTurnCancellationOutcome,
    Result, SandboxAgentAdmission, SandboxToolCall, SandboxToolCallReceipt, SequencedEvent, Store,
    TaskPlan, ToolApproval, ToolCallRecord, ToolCallResolution, TurnId, TurnRun, TurnSteerId,
};

use crate::error::ServerError;
use crate::principal::AuthContext;
use crate::state::AppState;

/// The durable store as one authenticated principal may see it.
///
/// Constructed only from server state plus a verified [`AuthContext`], so the
/// owner inside is always the one the auth middleware resolved. Fields stay
/// private and no method returns the inner handle.
#[derive(Clone)]
pub struct ScopedStore {
    store: Arc<dyn Store>,
    owner: OwnerId,
}

impl ScopedStore {
    /// Bind the state's store to the request's authenticated principal.
    pub(crate) fn new(state: &AppState, auth: &AuthContext) -> Self {
        Self {
            store: state.store.clone(),
            owner: auth.principal.owner_id(),
        }
    }

    // ------------------------------------------------------------------
    // Root aggregates — always owner-scoped. Another owner's row is
    // indistinguishable from a missing one.
    // ------------------------------------------------------------------

    /// Fetch the principal's chat by id.
    pub async fn get_chat(&self, id: ChatId) -> Result<Option<Chat>> {
        self.store.get_chat_scoped(&self.owner, id).await
    }

    /// The recurring route gate: the principal's chat, or a `404` that does
    /// not reveal whether someone else's chat exists under that id.
    pub async fn require_chat(&self, id: ChatId) -> std::result::Result<Chat, ServerError> {
        self.get_chat(id)
            .await?
            .ok_or_else(|| ServerError::not_found(format!("chat {id} not found")))
    }

    /// List the principal's chats, most-recently-created first.
    pub async fn list_chats(&self) -> Result<Vec<Chat>> {
        self.store.list_chats_scoped(&self.owner).await
    }

    /// Create a chat and apply related settings only if the chat insert
    /// succeeds.
    pub async fn create_chat_with_project_defaults_and_settings(
        &self,
        chat: &Chat,
        settings: &[(String, Value)],
    ) -> Result<Chat> {
        self.store
            .create_chat_with_project_defaults_and_settings_scoped(&self.owner, chat, settings)
            .await
    }

    /// Delete the principal's chat.
    pub async fn delete_chat(&self, id: ChatId) -> Result<DeleteChatOutcome> {
        self.store.delete_chat_scoped(&self.owner, id).await
    }

    /// The principal's chat transcript snapshot.
    pub async fn get_chat_transcript(&self, id: ChatId) -> Result<Option<ChatTranscriptSnapshot>> {
        self.store.get_chat_transcript_scoped(&self.owner, id).await
    }

    /// Update user-editable metadata on the principal's chat.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_chat_metadata(
        &self,
        id: ChatId,
        title: Option<Option<String>>,
        model: Option<Option<String>>,
        reasoning_effort: Option<Option<ReasoningEffort>>,
        permission_mode: Option<Option<PermissionMode>>,
        network_policy: Option<NetworkPolicy>,
    ) -> Result<bool> {
        self.store
            .update_chat_metadata_scoped(
                &self.owner,
                id,
                title,
                model,
                reasoning_effort,
                permission_mode,
                network_policy,
            )
            .await
    }

    /// Create a project owned by the principal.
    pub async fn create_project(&self, project: &Project) -> Result<()> {
        self.store.create_project_scoped(&self.owner, project).await
    }

    /// Fetch the principal's project by id.
    pub async fn get_project(&self, id: ProjectId) -> Result<Option<Project>> {
        self.store.get_project_scoped(&self.owner, id).await
    }

    /// The project counterpart of [`ScopedStore::require_chat`].
    pub async fn require_project(
        &self,
        id: ProjectId,
    ) -> std::result::Result<Project, ServerError> {
        self.get_project(id)
            .await?
            .ok_or_else(|| ServerError::not_found(format!("project {id} not found")))
    }

    /// List the principal's projects, most-recently-created first.
    pub async fn list_projects(&self) -> Result<Vec<Project>> {
        self.store.list_projects_scoped(&self.owner).await
    }

    /// Update the title of the principal's project.
    pub async fn update_project_title(&self, id: ProjectId, title: Option<String>) -> Result<bool> {
        self.store
            .update_project_title_scoped(&self.owner, id, title)
            .await
    }

    /// Delete the principal's project.
    pub async fn delete_project(&self, id: ProjectId) -> Result<DeleteProjectOutcome> {
        self.store.delete_project_scoped(&self.owner, id).await
    }

    /// File one of the principal's chats under one of their projects, or take
    /// it back out with `None`.
    pub async fn move_chat_to_project(
        &self,
        id: ChatId,
        project_id: Option<ProjectId>,
    ) -> Result<MoveChatOutcome> {
        self.store
            .move_chat_to_project_scoped(&self.owner, id, project_id)
            .await
    }

    /// Fetch the principal's document by id.
    pub async fn get_document(&self, id: DocumentId) -> Result<Option<DocumentRecord>> {
        self.store.get_document_scoped(&self.owner, id).await
    }

    /// List the principal's document summaries in one corpus scope.
    pub async fn list_document_summaries(
        &self,
        scope: DocumentScope,
        after: Option<DocumentListCursor>,
        limit: u64,
    ) -> Result<Vec<DocumentSummaryRecord>> {
        self.store
            .list_document_summaries_scoped(&self.owner, scope, after, limit)
            .await
    }

    /// Delete the principal's document.
    pub async fn delete_document(&self, id: DocumentId) -> Result<()> {
        self.store.delete_document_scoped(&self.owner, id).await
    }

    /// Accept a document source attributed to the principal; a parented
    /// document must name a parent the principal owns.
    pub async fn accept_document_source(
        &self,
        document: &DocumentSourceUpsert,
    ) -> Result<DocumentRecord> {
        self.store
            .accept_document_source_scoped(&self.owner, document)
            .await
    }

    // ------------------------------------------------------------------
    // Non-root operations — keyed by ids that hang off a root the handler
    // has already authorized through this view. Standing grants are the
    // exception: their reads and revocations are owner-scoped here, since
    // the grant list is a cross-chat surface with no per-request root to
    // authorize first. Settings stay deployment-scoped by design (#853) —
    // see the self-host section of `docs/how-openwave-works.md`.
    // ------------------------------------------------------------------

    /// [`Store::get_turn_run`].
    pub async fn get_turn_run(&self, id: TurnId) -> Result<Option<TurnRun>> {
        self.store.get_turn_run(id).await
    }

    /// [`Store::accept_turn_with_message_context`].
    #[allow(clippy::too_many_arguments)]
    pub async fn accept_turn_with_message_context(
        &self,
        id: TurnId,
        chat_id: ChatId,
        model: &str,
        content: &str,
        images: &[ImageRef],
        documents: &[DocumentId],
        invoked_skills: &[String],
        voice_input_used: bool,
    ) -> Result<AcceptTurnOutcome> {
        self.store
            .accept_turn_with_message_context(
                id,
                chat_id,
                model,
                content,
                images,
                documents,
                invoked_skills,
                voice_input_used,
            )
            .await
    }

    /// [`Store::accept_turn_steer_with_message_context`].
    #[allow(clippy::too_many_arguments)]
    pub async fn accept_turn_steer_with_message_context(
        &self,
        id: TurnSteerId,
        turn_id: TurnId,
        chat_id: ChatId,
        content: &str,
        invoked_skills: &[String],
        interrupt: bool,
        voice_input_used: bool,
    ) -> Result<AcceptTurnSteerOutcome> {
        self.store
            .accept_turn_steer_with_message_context(
                id,
                turn_id,
                chat_id,
                content,
                invoked_skills,
                interrupt,
                voice_input_used,
            )
            .await
    }

    /// [`Store::request_turn_cancellation_and_append_event`].
    pub async fn request_turn_cancellation_and_append_event(
        &self,
        id: TurnId,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<JournaledTurnOutcome<RequestTurnCancellationOutcome>>> {
        self.store
            .request_turn_cancellation_and_append_event(id, now)
            .await
    }

    /// [`Store::list_agent_runs`].
    pub async fn list_agent_runs(&self, chat_id: ChatId) -> Result<Vec<AgentRun>> {
        self.store.list_agent_runs(chat_id).await
    }

    /// [`Store::get_agent_run`].
    pub async fn get_agent_run(&self, id: AgentRunId) -> Result<Option<AgentRun>> {
        self.store.get_agent_run(id).await
    }

    /// [`Store::get_agent_run_task_plan`].
    pub async fn get_agent_run_task_plan(
        &self,
        id: AgentRunId,
    ) -> Result<Option<openwave_core::AgentRunTaskPlan>> {
        self.store.get_agent_run_task_plan(id).await
    }

    /// [`Store::get_agent_run_result`].
    pub async fn get_agent_run_result(&self, id: AgentRunId) -> Result<Option<AgentRunResult>> {
        self.store.get_agent_run_result(id).await
    }

    /// [`Store::request_agent_run_cancellation`].
    pub async fn request_agent_run_cancellation(
        &self,
        id: AgentRunId,
    ) -> Result<Option<RequestAgentRunCancellationOutcome>> {
        self.store.request_agent_run_cancellation(id).await
    }

    /// [`Store::list_agent_run_progress`].
    pub async fn list_agent_run_progress(
        &self,
        run_id: AgentRunId,
        after_sequence: i64,
        limit: u64,
    ) -> Result<Vec<openwave_core::AgentRunProgressEntry>> {
        self.store
            .list_agent_run_progress(run_id, after_sequence, limit)
            .await
    }

    /// [`Store::list_sandbox_tool_calls_for_agent_run`].
    pub async fn list_sandbox_tool_calls_for_agent_run(
        &self,
        agent_run_id: AgentRunId,
    ) -> Result<Vec<SandboxToolCall>> {
        self.store
            .list_sandbox_tool_calls_for_agent_run(agent_run_id)
            .await
    }

    /// [`Store::get_sandbox_agent_admission`].
    pub async fn get_sandbox_agent_admission(
        &self,
        agent_run_id: AgentRunId,
    ) -> Result<Option<SandboxAgentAdmission>> {
        self.store.get_sandbox_agent_admission(agent_run_id).await
    }

    /// [`Store::get_sandbox_tool_call_receipt`].
    pub async fn get_sandbox_tool_call_receipt(
        &self,
        call_id: CallId,
    ) -> Result<Option<SandboxToolCallReceipt>> {
        self.store.get_sandbox_tool_call_receipt(call_id).await
    }

    /// [`Store::list_pending_client_tool_calls`].
    pub async fn list_pending_client_tool_calls(
        &self,
        chat_id: ChatId,
    ) -> Result<Vec<ToolCallRecord>> {
        self.store.list_pending_client_tool_calls(chat_id).await
    }

    /// [`Store::list_pending_tool_call_approvals`].
    pub async fn list_pending_tool_call_approvals(
        &self,
        chat_id: ChatId,
        limit: u64,
    ) -> Result<Vec<ToolApproval>> {
        self.store
            .list_pending_tool_call_approvals(chat_id, limit)
            .await
    }

    /// Everything parked on the principal, across their own chats, oldest
    /// first. A cross-chat root read, so it is owner-scoped like the rest.
    pub async fn list_inbox_items(&self) -> Result<Vec<openwave_core::InboxItem>> {
        self.store.list_inbox_items_scoped(&self.owner).await
    }

    /// The standing grants reachable through the principal's own chats and
    /// projects, newest first.
    pub async fn list_standing_tool_grants(
        &self,
    ) -> Result<Vec<openwave_core::StandingGrantRecord>> {
        self.store
            .list_standing_tool_grants_scoped(&self.owner)
            .await
    }

    /// Withdraw one of the principal's standing grants. Someone else's grant
    /// is left standing and reports `false`, indistinguishable from absent.
    pub async fn revoke_standing_tool_grant(&self, source_call_id: CallId) -> Result<bool> {
        self.store
            .revoke_standing_tool_grant_scoped(&self.owner, source_call_id)
            .await
    }

    /// [`Store::list_events`].
    pub async fn list_events(&self, chat_id: ChatId, after: i64) -> Result<Vec<SequencedEvent>> {
        self.store.list_events(chat_id, after).await
    }

    /// [`Store::get_task_plan`].
    pub async fn get_task_plan(&self, chat_id: ChatId) -> Result<Option<TaskPlan>> {
        self.store.get_task_plan(chat_id).await
    }

    /// [`Store::list_pending_plan_approvals`].
    pub async fn list_pending_plan_approvals(
        &self,
        chat_id: ChatId,
    ) -> Result<Vec<PendingPlanApproval>> {
        self.store.list_pending_plan_approvals(chat_id).await
    }

    /// [`Store::decide_plan`].
    pub async fn decide_plan(
        &self,
        request: &DecidePlanRequest,
        decided_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<DecidePlanOutcome> {
        self.store.decide_plan(request, decided_at).await
    }

    /// [`Store::list_pending_user_questions`].
    pub async fn list_pending_user_questions(
        &self,
        chat_id: ChatId,
    ) -> Result<Vec<PendingUserQuestions>> {
        self.store.list_pending_user_questions(chat_id).await
    }

    /// [`Store::answer_user_questions`].
    pub async fn answer_user_questions(
        &self,
        request: &AnswerUserQuestionsRequest,
        answered_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<AnswerUserQuestionsOutcome> {
        self.store.answer_user_questions(request, answered_at).await
    }

    /// [`Store::list_message_attachments`].
    pub async fn list_message_attachments(
        &self,
        chat_id: ChatId,
    ) -> Result<Vec<MessageAttachment>> {
        self.store.list_message_attachments(chat_id).await
    }

    /// [`Store::list_tool_calls`].
    pub async fn list_tool_calls(&self, chat_id: ChatId) -> Result<Vec<ToolCallRecord>> {
        self.store.list_tool_calls(chat_id).await
    }

    /// [`Store::claim_client_tool_call`].
    pub async fn claim_client_tool_call(
        &self,
        id: CallId,
        chat_id: ChatId,
        executor_id: uuid::Uuid,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ClaimClientToolCallOutcome> {
        self.store
            .claim_client_tool_call(id, chat_id, executor_id, lease_token, now, lease_expires_at)
            .await
    }

    /// [`Store::heartbeat_client_tool_call`].
    pub async fn heartbeat_client_tool_call(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<HeartbeatClientToolCallOutcome> {
        self.store
            .heartbeat_client_tool_call(id, chat_id, lease_token, now, lease_expires_at)
            .await
    }

    /// [`Store::resolve_client_tool_call_and_append_event_with_rows`].
    #[allow(clippy::too_many_arguments)]
    pub async fn resolve_client_tool_call_and_append_event_with_rows(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
        rows: Option<&serde_json::Value>,
    ) -> Result<JournaledClientToolCallOutcome> {
        self.store
            .resolve_client_tool_call_and_append_event_with_rows(
                id,
                chat_id,
                lease_token,
                now,
                resolution,
                resolved_at,
                rows,
            )
            .await
    }

    /// [`Store::resolve_expired_client_tool_call_and_append_event_with_rows`].
    #[allow(clippy::too_many_arguments)]
    pub async fn resolve_expired_client_tool_call_and_append_event_with_rows(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
        rows: Option<&serde_json::Value>,
    ) -> Result<JournaledClientToolCallOutcome> {
        self.store
            .resolve_expired_client_tool_call_and_append_event_with_rows(
                id,
                chat_id,
                lease_token,
                now,
                resolution,
                resolved_at,
                rows,
            )
            .await
    }
}

impl FromRequestParts<AppState> for ScopedStore {
    /// Absence of an [`AuthContext`] means the auth middleware never ran for
    /// this route; no context, no store.
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> std::result::Result<Self, Self::Rejection> {
        let auth = AuthContext::from_request_parts(parts, state).await?;
        Ok(Self::new(state, &auth))
    }
}
