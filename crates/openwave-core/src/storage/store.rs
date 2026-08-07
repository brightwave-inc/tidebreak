use async_trait::async_trait;
use serde_json::Value;

use crate::approval::{ApprovalDecision, ApprovalRequest, StandingGrant, ToolApproval};
use crate::connected_app::{ConnectedApp, ConnectedAppKind};
use crate::deliverable::{CreateOutput, NewOutputRevision, OutputRecord, OutputRevision};
use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::{
    AgentRunId, AppId, AppRevisionId, CallId, ChatId, DocumentId, MessageId, OutputId,
    OutputRevisionId, ProjectId, RootAttachmentChangeId, TurnId, TurnSteerId,
};
use crate::image::ImageRef;
use crate::local_app::{AppGrant, AppRecord, AppRevision, CreateApp, NewAppRevision};
use crate::model::{
    AgentRun, AgentRunInboxEntry, AgentRunProgressEntry, AgentRunResult, AgentRunTier,
    AgentRunWaitSetCandidate, BeginRootAttachmentChange, BlobRetirement, BlobRetirementStatus,
    Chat, ClientToolCallRequest, DocumentListCursor, DocumentRecord, DocumentScope,
    DocumentSourceUpsert, DocumentSummaryRecord, DocumentUpsert, ExecFileRejection,
    ExecFileRejectionRecord, ExecFileSnapshot, ExecFileSnapshotRecord, Message, MessageAttachment,
    MessageDocumentAttachment, NetworkPolicy, OwnerId, PermissionMode, Project, ReasoningEffort,
    RootAttachmentChange, RootAttachmentChangeTerminal, ToolCallRecord, ToolCallResolution,
    TurnCheckpointProgress, TurnFailureRetry, TurnRun, TurnSteer,
};
use crate::provider::{RefusalOutcome, StopReason, Usage};
use crate::semantic_checkpoint::{ContextCheckpoint, SaveContextCheckpointOutcome};
use crate::{AnswerUserQuestionsRequest, PendingUserQuestions};

use super::types::*;

fn document_storage_unavailable<T>() -> Result<T> {
    Err(AgentError::Store(
        "document storage is not implemented by this Store".into(),
    ))
}

fn output_storage_unavailable<T>() -> Result<T> {
    Err(AgentError::Store(
        "output storage is not implemented by this Store".into(),
    ))
}

fn app_storage_unavailable<T>() -> Result<T> {
    Err(AgentError::Store(
        "local-app storage is not implemented by this Store".into(),
    ))
}

fn connected_app_storage_unavailable<T>() -> Result<T> {
    Err(AgentError::Store(
        "connected-app storage is not implemented by this Store".into(),
    ))
}

fn context_checkpoint_storage_unavailable<T>() -> Result<T> {
    Err(AgentError::Store(
        "durable context-checkpoint storage is not implemented by this Store".into(),
    ))
}

fn turn_storage_unavailable<T>() -> Result<T> {
    Err(AgentError::Store(
        "durable turn storage is not implemented by this Store".into(),
    ))
}

fn agent_run_storage_unavailable<T>() -> Result<T> {
    Err(AgentError::Store(
        "durable agent-run storage is not implemented by this Store".into(),
    ))
}

fn root_attachment_storage_unavailable<T>() -> Result<T> {
    Err(AgentError::Store(
        "durable root attachment storage is not implemented by this Store".into(),
    ))
}

fn operation_log_storage_unavailable<T>() -> Result<T> {
    Err(AgentError::Store(
        "durable operation-log storage is not implemented by this Store".into(),
    ))
}
/// Durable metadata and conversation state.
///
/// Implementations must be safe to share across threads (`Send + Sync`) and are
/// held behind `Arc<dyn Store>`, so this trait stays object-safe.
#[async_trait]
pub trait Store: Send + Sync {
    /// Persist a new project.
    async fn create_project(&self, project: &Project) -> Result<()>;

    /// Fetch a project by id, or `None` if it doesn't exist.
    async fn get_project(&self, id: ProjectId) -> Result<Option<Project>>;

    /// List projects, most-recently-created first.
    async fn list_projects(&self) -> Result<Vec<Project>>;

    /// Replace one project's human-facing title.
    ///
    /// Returns `false` when the project does not exist. Product adapters own
    /// title normalization and bounds before calling this storage primitive.
    async fn update_project_title(&self, _id: ProjectId, _title: Option<String>) -> Result<bool> {
        Err(AgentError::Store(
            "project metadata storage is not implemented by this Store".into(),
        ))
    }

    /// Remove one empty project without cascading owned product state.
    async fn delete_project(&self, _id: ProjectId) -> Result<DeleteProjectOutcome> {
        Err(AgentError::Store(
            "project deletion is not implemented by this Store".into(),
        ))
    }

    /// Persist a new authoritative document record.
    ///
    /// At most one of `chat_id` and `project_id` may be present, and it must
    /// identify an existing owner. A live document's ownership is immutable:
    /// callers must delete it before recreating the same id in another corpus.
    async fn create_document(&self, _document: &DocumentRecord) -> Result<()> {
        document_storage_unavailable()
    }

    /// Fetch an authoritative document by id, or `None` if it does not exist.
    async fn get_document(&self, _id: DocumentId) -> Result<Option<DocumentRecord>> {
        document_storage_unavailable()
    }

    /// List documents in `scope`, most-recently-created first.
    async fn list_documents(&self, _scope: DocumentScope) -> Result<Vec<DocumentRecord>> {
        document_storage_unavailable()
    }

    /// List document metadata in deterministic newest-first order.
    ///
    /// At most `limit` records are returned. When `after` is present, results
    /// begin strictly after its `(created_at, id)` tuple in descending display
    /// order. Implementations must not load canonical text.
    async fn list_document_summaries(
        &self,
        _scope: DocumentScope,
        _after: Option<DocumentListCursor>,
        _limit: u64,
    ) -> Result<Vec<DocumentSummaryRecord>> {
        document_storage_unavailable()
    }

    /// List document ids in `scope` without requiring canonical content.
    ///
    /// The default preserves compatibility for external stores; database-backed
    /// implementations should project only the id column for maintenance scans.
    async fn list_document_ids(&self, scope: DocumentScope) -> Result<Vec<DocumentId>> {
        Ok(self
            .list_documents(scope)
            .await?
            .into_iter()
            .map(|document| document.id)
            .collect())
    }

    /// Journal one turn's changes to granted folders and prune the chat's
    /// history back to its undo window.
    ///
    /// The prior bytes each record names must already be published to the blob
    /// store: the row is what makes them live, so a row committed ahead of its
    /// bytes points at nothing. Committing the rows cancels any retirement
    /// queued for those blobs, and drops the journal for turns outside the
    /// window, enqueueing whatever that frees.
    async fn record_exec_file_snapshots(
        &self,
        _chat_id: ChatId,
        _turn_id: TurnId,
        _files: &[ExecFileSnapshotRecord],
    ) -> Result<()> {
        document_storage_unavailable()
    }

    /// This chat's journaled file changes, newest first.
    async fn list_exec_file_snapshots(&self, _chat_id: ChatId) -> Result<Vec<ExecFileSnapshot>> {
        document_storage_unavailable()
    }

    /// Journal the staged files one turn could not safely materialize.
    async fn record_exec_file_rejections(
        &self,
        _chat_id: ChatId,
        _turn_id: TurnId,
        _files: &[ExecFileRejectionRecord],
    ) -> Result<()> {
        document_storage_unavailable()
    }

    /// This chat's rejected staged files, newest first.
    async fn list_exec_file_rejections(&self, _chat_id: ChatId) -> Result<Vec<ExecFileRejection>> {
        document_storage_unavailable()
    }

    /// Read the coalesced retirement state for one source blob.
    async fn get_blob_retirement(&self, _blob_id: uuid::Uuid) -> Result<Option<BlobRetirement>> {
        document_storage_unavailable()
    }

    /// Ensure an old filesystem orphan has a durable retirement candidate.
    ///
    /// Returns `true` only when a missing, succeeded, or cancelled episode was
    /// queued. Referenced blobs, active work, and exhausted failures are left
    /// unchanged. Filesystem auditors must hold the publisher/retirer blob guard.
    async fn ensure_orphan_blob_retirement(&self, _blob_id: uuid::Uuid) -> Result<bool> {
        document_storage_unavailable()
    }

    /// Claim the oldest effective-due blob retirement under a fresh lease.
    ///
    /// `lease_expires_at` must be after `now`. Expired running work is reclaimed
    /// with a new token and attempt; an expired final attempt becomes failed and
    /// the claim scan continues to the next candidate.
    async fn claim_blob_retirement(
        &self,
        _now: chrono::DateTime<chrono::Utc>,
        _lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<BlobRetirement>> {
        document_storage_unavailable()
    }

    /// Extend one exact live blob-retirement lease monotonically.
    async fn heartbeat_blob_retirement(
        &self,
        _blob_id: uuid::Uuid,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
        _lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        document_storage_unavailable()
    }

    /// Revalidate one exact live retirement lease immediately before deletion.
    ///
    /// This atomically cancels the retirement if an authoritative document
    /// reference exists. Callers must hold the same cross-process blob guard
    /// used by source publishers until deletion and resolution finish.
    async fn validate_blob_retirement_lease(
        &self,
        _blob_id: uuid::Uuid,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        document_storage_unavailable()
    }

    /// Mark one exact live blob-retirement lease as successfully deleted.
    ///
    /// Returns `false` if the row is no longer running under the exact,
    /// unexpired lease or `completed_at` would regress durable state.
    async fn complete_blob_retirement(
        &self,
        _blob_id: uuid::Uuid,
        _lease_token: uuid::Uuid,
        _completed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        document_storage_unavailable()
    }

    /// Record a deletion failure for one exact live blob-retirement lease.
    ///
    /// A future `retry_at` moves work with attempts remaining to `retry_wait`;
    /// no retry time, or an exhausted attempt budget, moves it to `failed`.
    /// Returns the resulting state, or `None` when the lease lost ownership.
    async fn record_blob_retirement_failure(
        &self,
        _blob_id: uuid::Uuid,
        _lease_token: uuid::Uuid,
        _failed_at: chrono::DateTime<chrono::Utc>,
        _retry_at: Option<chrono::DateTime<chrono::Utc>>,
        _error_code: &str,
        _error_detail: Option<&str>,
    ) -> Result<Option<BlobRetirementStatus>> {
        document_storage_unavailable()
    }

    /// Hard-delete source content.
    async fn delete_document(&self, _id: DocumentId) -> Result<()> {
        document_storage_unavailable()
    }

    /// Create or replace authoritative document content.
    ///
    /// Replacements preserve `created_at` and use last-write-wins semantics.
    /// `project_id`, when present, must identify an existing project. A live
    /// document cannot move between corpora.
    async fn upsert_document(&self, _document: &DocumentUpsert) -> Result<DocumentRecord> {
        document_storage_unavailable()
    }

    /// Atomically accept an already-published source blob and decoded text.
    async fn accept_document_source(
        &self,
        _document: &DocumentSourceUpsert,
    ) -> Result<DocumentRecord> {
        document_storage_unavailable()
    }

    /// Persist a new chat.
    ///
    /// The ordered projection must be valid. When `project_id` is set, the
    /// project must exist and the leading `project_default` roots must exactly
    /// snapshot its current ordered defaults. The insertion is atomic.
    async fn create_chat(&self, chat: &Chat) -> Result<()>;

    /// Persist a new chat while atomically deriving its project-default roots.
    ///
    /// `chat` must carry revision zero and an empty projection. Implementations
    /// resolve the current project inside the same atomic operation that inserts
    /// the chat, returning the exact persisted snapshot.
    async fn create_chat_with_project_defaults(&self, chat: &Chat) -> Result<Chat>;

    /// Fetch a chat by id, or `None` if it doesn't exist.
    async fn get_chat(&self, id: ChatId) -> Result<Option<Chat>>;

    /// List chats, most-recently-created first.
    async fn list_chats(&self) -> Result<Vec<Chat>>;

    /// Remove a conversation and its terminal product history atomically.
    ///
    /// This deliberately fails closed while any turn can still run, while any
    /// root remains attached, or while broker reconciliation is pending. The
    /// caller must first finish cancellation and use the durable root-detach
    /// flow; deletion never guesses at native broker state. Conversation-owned
    /// documents are removed and retained source blobs are enqueued for
    /// asynchronous retirement.
    async fn delete_chat(&self, _id: ChatId) -> Result<DeleteChatOutcome> {
        Err(AgentError::Store(
            "conversation deletion is not implemented by this Store".into(),
        ))
    }

    /// Atomically load a chat's durable messages and its event-journal
    /// watermark. Returns `None` when the chat does not exist.
    async fn get_chat_transcript(&self, id: ChatId) -> Result<Option<ChatTranscriptSnapshot>>;

    /// Set (or clear, with `None`) a chat's model override. A no-op if the chat
    /// doesn't exist.
    async fn set_chat_model(&self, id: ChatId, model: Option<String>) -> Result<()>;

    /// Set (or clear, with `None`) a chat's human-facing title. A no-op if the
    /// chat doesn't exist.
    async fn set_chat_title(&self, id: ChatId, title: Option<String>) -> Result<()>;

    /// Set a chat's title only while it has none, reporting whether it applied.
    ///
    /// This is the write a derived title must use. A user rename is the
    /// authoritative one, and it can land while a derived title is still being
    /// produced; an unconditional write would replace the name the user just
    /// typed with a guess. Whoever names the conversation first keeps it, which
    /// also makes renaming a chat the way to opt out of ever being renamed for.
    async fn set_chat_title_if_unset(&self, id: ChatId, title: &str) -> Result<bool>;

    /// Atomically update whichever user-editable chat metadata fields are
    /// present. An outer `None` leaves that field alone; an inner `None`
    /// clears it. Returns `false` if the chat does not exist.
    async fn update_chat_metadata(
        &self,
        id: ChatId,
        title: Option<Option<String>>,
        model: Option<Option<String>>,
        reasoning_effort: Option<Option<ReasoningEffort>>,
        permission_mode: Option<Option<PermissionMode>>,
        network_policy: Option<NetworkPolicy>,
    ) -> Result<bool>;

    // ------------------------------------------------------------------
    // Owner-scoped root surface (#853).
    //
    // Chats, projects, and documents are the ownership roots: everything
    // else hangs off one of them by id. The scoped variants below take the
    // requesting principal's durable [`OwnerId`] where the query is built,
    // so a row belonging to someone else is indistinguishable from a row
    // that does not exist. Request-facing callers (the server's routes) use
    // this surface; system paths that act on already-authorized ids (turn
    // workers, retirement scans) keep the unscoped methods.
    //
    // The defaults treat the store as single-owner — they ignore the owner
    // and delegate to the unscoped method, which is exact for the local
    // profile's in-process test stores. Any store that can hold rows for
    // more than one owner must override every method in this block; the
    // database-backed store does.
    // ------------------------------------------------------------------

    /// Persist a new chat owned by `owner`. When `chat.project_id` is set,
    /// the project must belong to the same owner.
    async fn create_chat_scoped(&self, owner: &OwnerId, chat: &Chat) -> Result<()> {
        let _ = owner;
        self.create_chat(chat).await
    }

    /// [`Store::create_chat_with_project_defaults`], attributing the chat to
    /// `owner` and resolving defaults only from a project of the same owner.
    async fn create_chat_with_project_defaults_scoped(
        &self,
        owner: &OwnerId,
        chat: &Chat,
    ) -> Result<Chat> {
        let _ = owner;
        self.create_chat_with_project_defaults(chat).await
    }

    /// Fetch `owner`'s chat by id; `None` when it does not exist **or**
    /// belongs to someone else.
    async fn get_chat_scoped(&self, owner: &OwnerId, id: ChatId) -> Result<Option<Chat>> {
        let _ = owner;
        self.get_chat(id).await
    }

    /// List `owner`'s chats, most-recently-created first.
    async fn list_chats_scoped(&self, owner: &OwnerId) -> Result<Vec<Chat>> {
        let _ = owner;
        self.list_chats().await
    }

    /// [`Store::delete_chat`] restricted to `owner`'s chats; someone else's
    /// chat reports [`DeleteChatOutcome::NotFound`].
    async fn delete_chat_scoped(&self, owner: &OwnerId, id: ChatId) -> Result<DeleteChatOutcome> {
        let _ = owner;
        self.delete_chat(id).await
    }

    /// [`Store::get_chat_transcript`] restricted to `owner`'s chats.
    async fn get_chat_transcript_scoped(
        &self,
        owner: &OwnerId,
        id: ChatId,
    ) -> Result<Option<ChatTranscriptSnapshot>> {
        let _ = owner;
        self.get_chat_transcript(id).await
    }

    /// [`Store::update_chat_metadata`] restricted to `owner`'s chats;
    /// someone else's chat reports `false`.
    #[allow(clippy::too_many_arguments)]
    async fn update_chat_metadata_scoped(
        &self,
        owner: &OwnerId,
        id: ChatId,
        title: Option<Option<String>>,
        model: Option<Option<String>>,
        reasoning_effort: Option<Option<ReasoningEffort>>,
        permission_mode: Option<Option<PermissionMode>>,
        network_policy: Option<NetworkPolicy>,
    ) -> Result<bool> {
        let _ = owner;
        self.update_chat_metadata(
            id,
            title,
            model,
            reasoning_effort,
            permission_mode,
            network_policy,
        )
        .await
    }

    /// Persist a new project owned by `owner`.
    async fn create_project_scoped(&self, owner: &OwnerId, project: &Project) -> Result<()> {
        let _ = owner;
        self.create_project(project).await
    }

    /// Fetch `owner`'s project by id; `None` when it does not exist or
    /// belongs to someone else.
    async fn get_project_scoped(&self, owner: &OwnerId, id: ProjectId) -> Result<Option<Project>> {
        let _ = owner;
        self.get_project(id).await
    }

    /// List `owner`'s projects, most-recently-created first.
    async fn list_projects_scoped(&self, owner: &OwnerId) -> Result<Vec<Project>> {
        let _ = owner;
        self.list_projects().await
    }

    /// [`Store::update_project_title`] restricted to `owner`'s projects.
    async fn update_project_title_scoped(
        &self,
        owner: &OwnerId,
        id: ProjectId,
        title: Option<String>,
    ) -> Result<bool> {
        let _ = owner;
        self.update_project_title(id, title).await
    }

    /// [`Store::delete_project`] restricted to `owner`'s projects; someone
    /// else's project reports [`DeleteProjectOutcome::NotFound`].
    async fn delete_project_scoped(
        &self,
        owner: &OwnerId,
        id: ProjectId,
    ) -> Result<DeleteProjectOutcome> {
        let _ = owner;
        self.delete_project(id).await
    }

    /// Fetch `owner`'s document by id; `None` when it does not exist or
    /// belongs to someone else.
    async fn get_document_scoped(
        &self,
        owner: &OwnerId,
        id: DocumentId,
    ) -> Result<Option<DocumentRecord>> {
        let _ = owner;
        self.get_document(id).await
    }

    /// [`Store::list_document_summaries`] restricted to `owner`'s documents.
    async fn list_document_summaries_scoped(
        &self,
        owner: &OwnerId,
        scope: DocumentScope,
        after: Option<DocumentListCursor>,
        limit: u64,
    ) -> Result<Vec<DocumentSummaryRecord>> {
        let _ = owner;
        self.list_document_summaries(scope, after, limit).await
    }

    /// [`Store::delete_document`] restricted to `owner`'s documents; someone
    /// else's document is left untouched, indistinguishable from absent.
    async fn delete_document_scoped(&self, owner: &OwnerId, id: DocumentId) -> Result<()> {
        let _ = owner;
        self.delete_document(id).await
    }

    /// [`Store::accept_document_source`] attributing a new standalone
    /// document to `owner`; a chat- or project-bound document must name a
    /// parent that belongs to `owner`.
    async fn accept_document_source_scoped(
        &self,
        owner: &OwnerId,
        document: &DocumentSourceUpsert,
    ) -> Result<DocumentRecord> {
        let _ = owner;
        self.accept_document_source(document).await
    }

    /// [`Store::list_standing_tool_grants`] restricted to grants whose chat
    /// or project belongs to `owner`. A grant's owner is its level's owner:
    /// grants are created inside an owner-scoped chat and cascade-deleted
    /// with their chat or project, so the derivation cannot drift.
    async fn list_standing_tool_grants_scoped(
        &self,
        owner: &OwnerId,
    ) -> Result<Vec<crate::approval::StandingGrantRecord>> {
        let _ = owner;
        self.list_standing_tool_grants().await
    }

    /// [`Store::revoke_standing_tool_grant`] restricted to `owner`'s grants;
    /// someone else's grant is left standing and reports `false`,
    /// indistinguishable from a grant that never existed.
    async fn revoke_standing_tool_grant_scoped(
        &self,
        owner: &OwnerId,
        source_call_id: CallId,
    ) -> Result<bool> {
        let _ = owner;
        self.revoke_standing_tool_grant(source_call_id).await
    }

    /// Create a conversation output together with its first revision.
    ///
    /// The caller has already written the revision's bytes to conversation
    /// private scratch under [`crate::deliverable::output_revision_relative_path`]
    /// and supplies their exact length and digest. Reusing `request.id` with
    /// identical content returns the original record so an ambiguous store
    /// response can be retried; reusing it with different content is rejected.
    ///
    /// At most one live output per conversation may carry a given filename. A
    /// creation that finds the name already taken fails with
    /// [`AgentError::OutputFilenameTaken`] naming the output that holds it, so
    /// the caller can revise that record instead of forking the name.
    async fn create_output(&self, _request: &CreateOutput) -> Result<OutputRecord> {
        output_storage_unavailable()
    }

    /// Append an immutable revision and publish it as the output's current one.
    ///
    /// The previous revision is retained and stays addressable by its own id,
    /// so an update can never destroy the bytes it replaced. Reusing
    /// `revision.id` with identical content is an exact retry.
    async fn append_output_revision(
        &self,
        _output_id: OutputId,
        _revision: &NewOutputRevision,
    ) -> Result<OutputRecord> {
        output_storage_unavailable()
    }

    /// Fetch one output by opaque id, including a soft-deleted one.
    async fn get_output(&self, _id: OutputId) -> Result<Option<OutputRecord>> {
        output_storage_unavailable()
    }

    /// List a conversation's live outputs, most recently updated first.
    async fn list_outputs(&self, _chat_id: ChatId, _limit: u64) -> Result<Vec<OutputRecord>> {
        output_storage_unavailable()
    }

    /// List a conversation's live outputs carrying one exact filename, most
    /// recently updated first.
    ///
    /// Filename is the identity everything outside the store works with, and a
    /// conversation can hold more outputs than any bounded listing returns, so
    /// resolving a name asks for the name rather than paging the catalog and
    /// hoping it is on the page.
    async fn find_outputs_by_filename(
        &self,
        _chat_id: ChatId,
        _filename: &str,
    ) -> Result<Vec<OutputRecord>> {
        output_storage_unavailable()
    }

    /// List one output's revisions, newest first.
    async fn list_output_revisions(&self, _output_id: OutputId) -> Result<Vec<OutputRevision>> {
        output_storage_unavailable()
    }

    /// Fetch one revision by opaque id.
    async fn get_output_revision(&self, _id: OutputRevisionId) -> Result<Option<OutputRevision>> {
        output_storage_unavailable()
    }

    /// Soft-delete an output, hiding it from the catalog while retaining its
    /// revisions. Returns `false` only when the output does not exist; deleting
    /// an already-deleted output is the same durable outcome, not a conflict.
    async fn delete_output(
        &self,
        _id: OutputId,
        _deleted_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        output_storage_unavailable()
    }

    /// Restore a soft-deleted output, returning it to the catalog. This is the
    /// exact inverse of [`Store::delete_output`], so retracting a submitted
    /// output is reversible. Returns `false` only when the output does not
    /// exist; restoring a live output is the same durable outcome, not a
    /// conflict. Nothing about the revision history changes.
    ///
    /// A retraction frees the output's filename, so restoring fails with
    /// [`AgentError::OutputFilenameTaken`] when something else has since
    /// claimed it — the caller retracts the current holder first.
    async fn restore_output(
        &self,
        _id: OutputId,
        _restored_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        output_storage_unavailable()
    }

    /// Republish an existing revision of an output as its current one.
    ///
    /// This is the revert primitive: it moves the current-revision pointer to
    /// any revision already recorded for the output without appending or
    /// destroying anything, so it is fully reversible. The revision must belong
    /// to the output, and the output must be live. The revision count is
    /// unchanged; only the current pointer and update time move.
    async fn set_current_output_revision(
        &self,
        _output_id: OutputId,
        _revision_id: OutputRevisionId,
        _updated_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<OutputRecord> {
        output_storage_unavailable()
    }

    /// Create a profile-scoped local app together with its first revision.
    ///
    /// The caller has already published the bundle bytes under the profile
    /// data directory at [`crate::local_app::app_revision_relative_path`] and
    /// supplies their exact length and digest; the manifest is validated
    /// structurally before anything is stored. Reusing `request.id` with
    /// identical content returns the original record so an ambiguous store
    /// response can be retried; reusing it with different content is rejected.
    async fn create_app(&self, _request: &CreateApp) -> Result<AppRecord> {
        app_storage_unavailable()
    }

    /// Append an immutable revision and publish it as the app's current one.
    ///
    /// The previous revision is retained and stays addressable by its own id,
    /// so an update can never destroy the bundle it replaced. Reusing
    /// `revision.id` with identical content is an exact retry; reaching the
    /// revision cap refuses the write rather than dropping history.
    async fn append_app_revision(
        &self,
        _app_id: AppId,
        _revision: &NewAppRevision,
    ) -> Result<AppRecord> {
        app_storage_unavailable()
    }

    /// Fetch one app by opaque id, including a soft-deleted one.
    async fn get_app(&self, _id: AppId) -> Result<Option<AppRecord>> {
        app_storage_unavailable()
    }

    /// List the profile's live apps, most recently updated first.
    async fn list_apps(&self, _limit: u64) -> Result<Vec<AppRecord>> {
        app_storage_unavailable()
    }

    /// List one app's revisions, newest first.
    async fn list_app_revisions(&self, _app_id: AppId) -> Result<Vec<AppRevision>> {
        app_storage_unavailable()
    }

    /// Fetch one app revision by opaque id.
    async fn get_app_revision(&self, _id: AppRevisionId) -> Result<Option<AppRevision>> {
        app_storage_unavailable()
    }

    /// Soft-delete an app, hiding it from the library while retaining its
    /// revisions. Returns `false` only when the app does not exist; deleting
    /// an already-deleted app is the same durable outcome, not a conflict.
    async fn delete_app(
        &self,
        _id: AppId,
        _deleted_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        app_storage_unavailable()
    }

    /// Restore a soft-deleted app, returning it to the library. The exact
    /// inverse of [`Store::delete_app`]; the revision history is untouched.
    /// Returns `false` only when the app does not exist; restoring a live app
    /// is the same durable outcome, not a conflict.
    async fn restore_app(
        &self,
        _id: AppId,
        _restored_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        app_storage_unavailable()
    }

    /// Record explicit user consent for one app, replacing any previous grant.
    ///
    /// The grant is host-computed from the app's current manifest and the
    /// server definitions current at consent time; implementations validate
    /// its bindings with the manifest grammar and refuse a missing or deleted
    /// app. There is at most one grant per app.
    async fn put_app_grant(&self, _grant: &AppGrant) -> Result<()> {
        app_storage_unavailable()
    }

    /// Fetch one app's grant, when the user has consented and not revoked.
    async fn get_app_grant(&self, _app_id: AppId) -> Result<Option<AppGrant>> {
        app_storage_unavailable()
    }

    /// Revoke one app's grant. Returns `false` when no grant existed;
    /// revoking twice is the same durable outcome, not a conflict.
    async fn delete_app_grant(&self, _app_id: AppId) -> Result<bool> {
        app_storage_unavailable()
    }

    /// Every app grant whose owning app is still in the library (not
    /// soft-deleted), in no particular order.
    ///
    /// Serves aggregate views over consent — e.g. how many apps currently
    /// bind one connected app — where fetching grants one app at a time
    /// would mean walking the whole library. A grant of a deleted app is
    /// excluded: it can no longer be exercised, so surfaces built on this
    /// must not count it.
    async fn list_live_app_grants(&self) -> Result<Vec<AppGrant>> {
        app_storage_unavailable()
    }

    /// List every connected app the profile holds, oldest first.
    ///
    /// Kind-specific definitions come back as the bounded JSON the owning
    /// layer stored; callers parse per kind and fail closed per record.
    async fn list_connected_apps(&self) -> Result<Vec<ConnectedApp>> {
        connected_app_storage_unavailable()
    }

    /// Replace the profile's connected apps of one kind wholesale.
    ///
    /// Mirrors the settings surfaces that edit a complete list: rows of
    /// `kind` absent from `apps` are deleted, present ids are updated in
    /// place (keeping their `created_at`), and new ids are inserted. Records
    /// of other kinds are untouched. Implementations validate each record's
    /// kind-independent contract and refuse a mixed-kind call.
    async fn replace_connected_apps(
        &self,
        _kind: ConnectedAppKind,
        _apps: &[ConnectedApp],
    ) -> Result<()> {
        connected_app_storage_unavailable()
    }

    /// Persist the next versioned context checkpoint for one conversation.
    ///
    /// Implementations verify that the inclusive source-message boundary
    /// belongs to `checkpoint.chat_id`, and serialize writes per chat. An exact
    /// retry recovers the durable record; stale and conflicting rewrites are
    /// returned as typed outcomes instead of replacing newer context.
    async fn save_context_checkpoint(
        &self,
        _checkpoint: &ContextCheckpoint,
    ) -> Result<SaveContextCheckpointOutcome> {
        context_checkpoint_storage_unavailable()
    }

    /// Fetch the one current semantic checkpoint for a conversation.
    ///
    /// This record is intentionally distinct from visible messages. Consumers
    /// that later project it into a provider request must treat it as bounded,
    /// untrusted historical data rather than as a capability grant.
    async fn get_context_checkpoint(&self, _chat_id: ChatId) -> Result<Option<ContextCheckpoint>> {
        context_checkpoint_storage_unavailable()
    }

    /// Atomically begin one exact broker-backed attachment change.
    ///
    /// Implementations validate `request`, lock authoritative chat/projection
    /// state, derive broker subject and prior projection metadata, enforce one
    /// awaiting change per chat, and durably project intent before returning.
    /// Transport adapters must derive `executor_id` from authenticated native
    /// control; it is not renderer-selected authorization.
    async fn begin_root_attachment_change(
        &self,
        _request: &BeginRootAttachmentChange,
    ) -> Result<BeginRootAttachmentChangeOutcome> {
        root_attachment_storage_unavailable()
    }

    /// Atomically finish one exact change under its stable executor.
    ///
    /// Exact terminal retries return `Existing`. Implementations apply the
    /// final projection, terminal receipt, and result revision together. The
    /// server-owned finish time is clamped to the immutable creation time under
    /// the operation lock so wall-clock skew cannot wedge pending work.
    /// Adapters must first bind the broker receipt to this exact persisted
    /// operation; arbitrary transport failures are not durable broker failures.
    async fn finish_root_attachment_change(
        &self,
        _id: RootAttachmentChangeId,
        _executor_id: uuid::Uuid,
        _terminal: &RootAttachmentChangeTerminal,
        _finished_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<FinishRootAttachmentChangeOutcome> {
        root_attachment_storage_unavailable()
    }

    /// Fetch one attachment change by exact idempotency identity.
    async fn get_root_attachment_change(
        &self,
        _id: RootAttachmentChangeId,
    ) -> Result<Option<RootAttachmentChange>> {
        root_attachment_storage_unavailable()
    }

    /// List up to `limit` awaiting changes owned by one stable native executor.
    ///
    /// `limit` must be in `1..=MAX_PENDING_ROOT_ATTACHMENT_CHANGES` and results
    /// are returned in deterministic oldest-first order.
    async fn list_pending_root_attachment_changes(
        &self,
        _executor_id: uuid::Uuid,
        _limit: u64,
    ) -> Result<Vec<RootAttachmentChange>> {
        root_attachment_storage_unavailable()
    }

    /// Atomically accept one foreground coordinator or sandboxed child run.
    ///
    /// `id` is the run's stable idempotency identity. Foreground runs require no
    /// parent, spawn call, or input and become active immediately. Sandboxed
    /// runs require a unique `spawn_call_id`, non-empty task, and active
    /// depth-zero foreground parent in the same chat; they are accepted as
    /// queued depth-one work. An exact spawn-call retry recovers the original
    /// run even if the caller supplies a fresh run id. Recursive children are
    /// rejected by construction.
    async fn accept_agent_run(
        &self,
        _id: AgentRunId,
        _chat_id: ChatId,
        _parent_id: Option<AgentRunId>,
        _spawn_call_id: Option<CallId>,
        _tier: AgentRunTier,
        _input: Option<&str>,
    ) -> Result<AcceptAgentRunOutcome> {
        agent_run_storage_unavailable()
    }

    /// Admit one depth-one sandbox child without advancing its origin turn.
    ///
    /// The child id is derived from `spawn_call_id`; callers cannot choose a
    /// second identity for the same model request. The origin turn, foreground
    /// parent, child, and immutable admission receipt commit together under the
    /// chat/turn write lock. Existing exact receipts are recovered before the
    /// bounded per-chat active-run check, making an ambiguous commit retry safe.
    /// A non-blocking checkpoint may additionally bind one exact root-relative
    /// file identity after validating its root against the locked chat
    /// attachment projection; the receipt itself grants no host authority.
    /// The stronger checkpoint boundary below composes this admission with the
    /// foreground transcript, progress, event, and immediate continuation.
    #[allow(clippy::too_many_arguments)]
    async fn admit_sandbox_agent_run(
        &self,
        _origin_turn_id: TurnId,
        _spawn_call_id: CallId,
        _input: &str,
        _lease_token: uuid::Uuid,
        _expected_steer_revision: i64,
        _max_active_background_agents: u32,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<AdmitSandboxAgentRunOutcome>> {
        agent_run_storage_unavailable()
    }

    /// Admit one depth-one sandbox child that executes inside a sandbox-resident
    /// container rather than in process.
    ///
    /// Identical to [`Store::admit_sandbox_agent_run`] except the child's
    /// [`AgentRunExecutionLocation`](crate::model::AgentRunExecutionLocation) is
    /// `Container`, so the in-process scheduler leaves it and the
    /// sandbox-resident driver claims it with
    /// [`Store::claim_container_agent_run`], provisions a container, attaches,
    /// proxies model inference back over the reverse channel, and commits the
    /// result through the same fenced result path.
    #[allow(clippy::too_many_arguments)]
    async fn admit_sandbox_container_agent_run(
        &self,
        _origin_turn_id: TurnId,
        _spawn_call_id: CallId,
        _input: &str,
        _lease_token: uuid::Uuid,
        _expected_steer_revision: i64,
        _max_active_background_agents: u32,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<AdmitSandboxAgentRunOutcome>> {
        agent_run_storage_unavailable()
    }

    /// Claim one specific queued sandbox-resident container run by id under an
    /// exact bounded lease.
    ///
    /// The sandbox-resident driver calls this for the exact run it is
    /// provisioning a container for; unlike [`Store::claim_agent_run`], which the
    /// in-process scheduler uses to select the oldest due in-process run, this
    /// only transitions a fresh `queued` `container` run to `running`. Reusing
    /// `lease_token` recovers its original still-live claim and never claims
    /// different work. The returned lease fences the run's result commit exactly
    /// as an in-process claim does. Refuses — leaving the run queued — while
    /// `max_running_containers` container runs are already running; container
    /// runs bypass the in-process scheduler's limits, so this claim is where
    /// their own bound is enforced.
    async fn claim_container_agent_run(
        &self,
        _id: AgentRunId,
        _lease_token: uuid::Uuid,
        _lease_duration: chrono::Duration,
        _max_running_containers: u32,
    ) -> Result<Option<AgentRun>> {
        agent_run_storage_unavailable()
    }

    /// List bounded oldest-first candidates for the container-run worker.
    ///
    /// This scan is only latency and recovery plumbing: every returned id must
    /// still pass [`Store::claim_container_agent_run`]'s transactional status,
    /// deadline, admission, and concurrency checks before any container is
    /// provisioned.
    async fn list_container_agent_run_candidates(&self, _limit: u64) -> Result<Vec<AgentRunId>> {
        agent_run_storage_unavailable()
    }

    /// List container-located runs whose driver died: `running` under an
    /// expired lease with the deadline still open. The in-process lease reaper
    /// deliberately exempts container runs, so this scan feeds the recovery
    /// pass that replaces it.
    async fn list_reclaimable_container_agent_runs(
        &self,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<AgentRun>> {
        agent_run_storage_unavailable()
    }

    /// Reclaim one expired-lease container run under a fresh bounded lease,
    /// **without** a second execution attempt: exactly one container was ever
    /// asked to run it, so recovery re-drives that same attempt through the
    /// durable provisioning record and the operation log. Refuses a live lease
    /// and a crossed deadline. Reusing `lease_token` recovers only its original
    /// still-live claim.
    async fn reclaim_container_agent_run(
        &self,
        _id: AgentRunId,
        _lease_token: uuid::Uuid,
        _lease_duration: chrono::Duration,
    ) -> Result<Option<AgentRun>> {
        agent_run_storage_unavailable()
    }

    /// Commit a durable provisioning intent for one container run, before the
    /// backend is asked to create anything. Returns the existing record instead
    /// when one is already present, so a restarted host reconciles rather than
    /// provisioning a second sandbox for the same single-attempt run.
    ///
    /// `admission` is the run's durable admission decision, recorded on the
    /// intent so it survives crashes and disconnects; every later derivation
    /// of the sandbox's admission mode reads the record, never the caller.
    async fn begin_sandbox_provision(
        &self,
        _run_id: uuid::Uuid,
        _tag: &str,
        _window_expires_at: chrono::DateTime<chrono::Utc>,
        _admission: SandboxAdmissionMode,
    ) -> Result<BeginSandboxProvisionOutcome> {
        agent_run_storage_unavailable()
    }

    /// Commit the backend's handle onto the run's `Intended` record. Returns
    /// `false` if the record is no longer `Intended` — the window lapsed and the
    /// sweep claimed it first — in which case the caller owns a sandbox the
    /// durable state has already disowned and must destroy it.
    async fn commit_sandbox_provision_handle(
        &self,
        _run_id: uuid::Uuid,
        _handle: &str,
    ) -> Result<bool> {
        agent_run_storage_unavailable()
    }

    /// Move one run's provisioning record to `Teardown`, whatever non-`Done`
    /// state it is in, returning it. `None` if no record exists or the sandbox
    /// is already confirmed gone.
    async fn enqueue_sandbox_teardown(
        &self,
        _run_id: uuid::Uuid,
    ) -> Result<Option<SandboxProvision>> {
        agent_run_storage_unavailable()
    }

    /// Mark one run's `Teardown` record `Done` after its destroy confirmed.
    async fn complete_sandbox_teardown(&self, _run_id: uuid::Uuid) -> Result<()> {
        agent_run_storage_unavailable()
    }

    /// Move every `Intended` record whose window lapsed before `now` to
    /// `Teardown`, returning the lapsed records. The admission failed on the
    /// intent whether or not a create ever reached the provider; the tag sweep
    /// reclaims whatever the provider holds under those tags.
    async fn lapse_sandbox_provisions(
        &self,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<SandboxProvision>> {
        agent_run_storage_unavailable()
    }

    /// Every record currently owing a teardown.
    async fn list_sandbox_teardowns(&self) -> Result<Vec<SandboxProvision>> {
        agent_run_storage_unavailable()
    }

    /// One run's provisioning record, if any.
    async fn get_sandbox_provision(&self, _run_id: uuid::Uuid) -> Result<Option<SandboxProvision>> {
        agent_run_storage_unavailable()
    }

    /// Retain a well-formed result that failed the fenced commit predicate —
    /// the run was already terminal or the lease was gone — as
    /// non-authoritative evidence on the provisioning record. First writer
    /// wins; returns whether this call retained it. Never commits anything.
    async fn record_late_container_result_evidence(
        &self,
        _run_id: uuid::Uuid,
        _text: &str,
    ) -> Result<bool> {
        agent_run_storage_unavailable()
    }

    /// The correlation tags of every live provisioning record — `Intended`
    /// within its window plus `Committed` — the set the orphan sweep must not
    /// reclaim. An `Intended` tag stays live until [`lapse_sandbox_provisions`]
    /// moves it, so the sweep can never race a slow in-flight create.
    ///
    /// [`lapse_sandbox_provisions`]: Store::lapse_sandbox_provisions
    async fn live_sandbox_tags(&self) -> Result<Vec<String>> {
        agent_run_storage_unavailable()
    }

    /// Atomically admit one depth-one sandbox child and yield the foreground
    /// turn at a non-blocking spawn boundary.
    ///
    /// Exact receipt recovery runs before mutable lease and steering checks.
    /// A successful transition writes one terminal, non-executable
    /// orchestration tool call and its `ToolCallCompleted` event, applies one
    /// progress delta, then moves `running` to `resuming` with no live lease.
    /// Admission counts nonterminal background runs across the whole chat
    /// against the request's settings-resolved limit.
    /// Foreground orchestration advertises this together with the explicit
    /// ordered wait boundary; sandbox agents receive neither contract.
    async fn checkpoint_sandbox_spawn(
        &self,
        _request: &crate::model::SandboxSpawnCheckpointRequest,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<CheckpointSandboxSpawnOutcome>> {
        agent_run_storage_unavailable()
    }

    /// The still-ungated tail of the spawn batch the previous claim segment
    /// parked on, for the claim that resumes it.
    ///
    /// A model step can name several delegations at once and each one is
    /// approved on its own, so the turn parks once per admitted spawn. The
    /// siblings that have not reached the gate travel with the checkpoint
    /// rather than being re-derived from the model, which keeps every spawn on
    /// the call id it was streamed with and spends no provider call per
    /// approval. Returns empty for any claim that is not the immediate
    /// successor of a spawn park.
    ///
    /// Every store states this capability explicitly. A store whose
    /// [`checkpoint_sandbox_spawn`](Store::checkpoint_sandbox_spawn) remains
    /// unavailable returns an empty batch; a store that can checkpoint spawns
    /// must recover the carried tail or return an error rather than silently
    /// dropping it.
    async fn resumed_sandbox_spawn_batch(
        &self,
        turn_id: TurnId,
        attempt_count: i32,
        claim_count: i32,
    ) -> Result<Vec<crate::agent::SandboxAgentSpawnRequest>>;

    /// Fetch immutable origin ownership for an admitted sandbox child.
    async fn get_sandbox_agent_admission(
        &self,
        _child_run_id: AgentRunId,
    ) -> Result<Option<crate::model::SandboxAgentAdmission>> {
        agent_run_storage_unavailable()
    }

    /// Fetch one agent run by its exact idempotency identity.
    async fn get_agent_run(&self, _id: AgentRunId) -> Result<Option<AgentRun>> {
        agent_run_storage_unavailable()
    }

    /// List a chat's runs in deterministic creation order.
    async fn list_agent_runs(&self, _chat_id: ChatId) -> Result<Vec<AgentRun>> {
        agent_run_storage_unavailable()
    }

    /// Fetch the immutable terminal receipt for one agent run, if it exists.
    async fn get_agent_run_result(&self, _id: AgentRunId) -> Result<Option<AgentRunResult>> {
        agent_run_storage_unavailable()
    }

    /// Append one bounded progress line to a background run's ordered stream,
    /// assigning it the next per-run sequence.
    ///
    /// `source_key` is the producer's own identity for the line — a sandbox
    /// protocol event sequence, or the durable checkpoint a model preamble
    /// belongs to. Re-appending a key that already exists is a no-op, so a
    /// reattached container redelivering events and a worker retrying an
    /// ambiguous commit both leave one line rather than two.
    ///
    /// Text longer than [`AgentRunProgressEntry::MAX_TEXT_LEN`] is truncated on
    /// a character boundary rather than refused; a line is observation, and a
    /// truncated one still tells the reader more than a dropped one. Retention
    /// bounds the stream to [`AgentRunProgressEntry::RETAINED_PER_RUN`] lines.
    async fn append_agent_run_progress(
        &self,
        _run_id: AgentRunId,
        _source_key: &str,
        _text: &str,
    ) -> Result<()> {
        agent_run_storage_unavailable()
    }

    /// Read one run's progress lines strictly newer than `after_sequence`, in
    /// ascending order, bounded by `limit`.
    ///
    /// A cursor of zero starts at the beginning of whatever retention still
    /// holds. This is a read model: it takes no lease and never mutates.
    async fn list_agent_run_progress(
        &self,
        _run_id: AgentRunId,
        _after_sequence: i64,
        _limit: u64,
    ) -> Result<Vec<AgentRunProgressEntry>> {
        agent_run_storage_unavailable()
    }

    /// Atomically claim the oldest due sandbox run under exact bounded lease
    /// ownership.
    ///
    /// The global scheduler lock makes global and per-chat concurrency limits
    /// race-safe across processes. Expired leases are reclaimed only while the
    /// attempt budget remains; exhausted attempts and wall-clock deadlines are
    /// terminalized before scanning continues. Reusing `lease_token` recovers
    /// only its original still-live claim and can never claim different work.
    async fn claim_agent_run(
        &self,
        _lease_token: uuid::Uuid,
        _lease_duration: chrono::Duration,
        _max_running_global: u32,
        _max_running_per_chat: u32,
    ) -> Result<Option<AgentRun>> {
        agent_run_storage_unavailable()
    }

    /// Monotonically extend one exact live sandbox lease without resurrecting
    /// expiry or crossing the run's absolute deadline.
    async fn heartbeat_agent_run(
        &self,
        _id: AgentRunId,
        _lease_token: uuid::Uuid,
        _lease_duration: chrono::Duration,
    ) -> Result<bool> {
        agent_run_storage_unavailable()
    }

    /// Atomically accept one model step's canonical sandbox tool arguments,
    /// record the exact originating sandbox lease, and release that lease into
    /// `waiting`. Exact retries recover the checkpoint after the calls resolve.
    ///
    /// The step's calls are one durable batch: they share a park lease, carry
    /// their emission order as `batch_ordinal`, and the run resumes only once
    /// every one of them is terminal.
    ///
    /// An entry carrying a resolution lands terminal with its receipt in the
    /// same transaction; a batch in which every entry does releases the lease
    /// into `retry_wait` instead, since no executor lane will see any of them.
    async fn park_agent_run_for_sandbox_tool_calls(
        &self,
        _agent_run_id: AgentRunId,
        _lease_token: uuid::Uuid,
        _entries: &[crate::model::SandboxToolCallParkEntry],
    ) -> Result<ParkSandboxToolCallOutcome> {
        agent_run_storage_unavailable()
    }

    /// Claim one accepted sandbox tool call under an exact expiring executor
    /// lease. The executor token is a capability and is never included in
    /// ordinary history reads.
    async fn claim_sandbox_tool_call(
        &self,
        _id: CallId,
        _lease_token: uuid::Uuid,
        _lease_duration: chrono::Duration,
    ) -> Result<ClaimSandboxToolCallOutcome> {
        agent_run_storage_unavailable()
    }

    /// Claim one accepted sandbox call only when its immutable tool name is
    /// exactly `name`. Executors use this filtered authority so one tool lane
    /// can never terminalize another tool's durable work.
    async fn claim_sandbox_tool_call_named(
        &self,
        _id: CallId,
        _name: &str,
        _lease_token: uuid::Uuid,
        _lease_duration: chrono::Duration,
    ) -> Result<ClaimSandboxToolCallOutcome> {
        agent_run_storage_unavailable()
    }

    /// Claim only the fixed delegated-file tool and atomically recover its
    /// pathless-root authority from a still-attached immutable admission.
    async fn claim_delegated_file_read(
        &self,
        _id: CallId,
        _lease_token: uuid::Uuid,
        _lease_duration: chrono::Duration,
    ) -> Result<ClaimDelegatedFileReadOutcome> {
        agent_run_storage_unavailable()
    }

    /// Extend a live executor lease only for the fixed delegated-file lane.
    async fn heartbeat_delegated_file_read(
        &self,
        _id: CallId,
        _lease_token: uuid::Uuid,
        _lease_duration: chrono::Duration,
    ) -> Result<Option<chrono::Duration>> {
        agent_run_storage_unavailable()
    }

    /// Resolve a live executor lease only for the fixed delegated-file lane.
    async fn resolve_delegated_file_read(
        &self,
        _id: CallId,
        _lease_token: uuid::Uuid,
        _resolution: &ToolCallResolution,
    ) -> Result<ResolveSandboxToolCallOutcome> {
        agent_run_storage_unavailable()
    }

    /// Revalidate one exact live sandbox-tool executor lease against the
    /// database clock and extend it up to its sandbox run deadline.
    ///
    /// This is the final cancellation/deadline fence before an executor may
    /// begin an external operation. `None` means cancellation, expiry, a
    /// terminal receipt, or a competing executor already won. `Some` returns
    /// the remaining lease budget calculated from the same database-clock
    /// transaction, so an executor need not compare host wall time to a stored
    /// absolute expiry.
    async fn heartbeat_sandbox_tool_call(
        &self,
        _id: CallId,
        _lease_token: uuid::Uuid,
        _lease_duration: chrono::Duration,
    ) -> Result<Option<chrono::Duration>> {
        agent_run_storage_unavailable()
    }

    /// Park a claimed sandbox tool call for its single bounded retry under
    /// the exact live executor lease. The call moves to `retry_wait` with a
    /// `retry_at` of the database clock plus `delay`, releases its executor
    /// lease, and becomes claimable again once `retry_at` passes; its waiting
    /// sandbox run is untouched. A call that already spent its retry cannot be
    /// parked again — that is an executor invariant breach, not a race.
    async fn retry_sandbox_tool_call(
        &self,
        _id: CallId,
        _lease_token: uuid::Uuid,
        _delay: chrono::Duration,
    ) -> Result<RetrySandboxToolCallOutcome> {
        agent_run_storage_unavailable()
    }

    /// Atomically write one immutable terminal receipt under the exact live
    /// executor lease and make its sandbox run claimable for continuation.
    /// Exact ambiguous retries recover the same receipt.
    async fn resolve_sandbox_tool_call(
        &self,
        _id: CallId,
        _lease_token: uuid::Uuid,
        _resolution: &ToolCallResolution,
    ) -> Result<ResolveSandboxToolCallOutcome> {
        agent_run_storage_unavailable()
    }

    /// Resolve one `update_task_plan` checkpoint and commit the plan it
    /// recorded in the same transaction.
    ///
    /// Steps must already be validated at the tool boundary; storage records
    /// what it is given. The plan is keyed by the checkpoint's own run, so
    /// sandbox siblings never overwrite each other or the chat's plan.
    async fn resolve_sandbox_task_plan_call(
        &self,
        _id: CallId,
        _lease_token: uuid::Uuid,
        _steps: &[crate::TaskPlanStep],
        _resolution: &ToolCallResolution,
    ) -> Result<ResolveSandboxToolCallOutcome> {
        agent_run_storage_unavailable()
    }

    /// One background run's current task plan, or `None` when it made none.
    async fn get_agent_run_task_plan(
        &self,
        _agent_run_id: AgentRunId,
    ) -> Result<Option<crate::AgentRunTaskPlan>> {
        agent_run_storage_unavailable()
    }

    /// Fetch a sandbox tool checkpoint by its stable model-visible identity.
    async fn get_sandbox_tool_call(
        &self,
        _id: CallId,
    ) -> Result<Option<crate::model::SandboxToolCall>> {
        agent_run_storage_unavailable()
    }

    /// Fetch the immutable terminal receipt, if sandbox tool work resolved.
    async fn get_sandbox_tool_call_receipt(
        &self,
        _id: CallId,
    ) -> Result<Option<crate::model::SandboxToolCallReceipt>> {
        agent_run_storage_unavailable()
    }

    /// List immutable sandbox tool checkpoints for one isolated run in creation
    /// order. A resumed sandbox rebuilds only its own tool transcript from
    /// these durable records and their terminal receipts.
    async fn list_sandbox_tool_calls_for_agent_run(
        &self,
        _agent_run_id: AgentRunId,
    ) -> Result<Vec<crate::model::SandboxToolCall>> {
        agent_run_storage_unavailable()
    }

    /// List bounded oldest-first accepted work and expired claims for durable
    /// executor recovery. Claiming remains the authority for ownership.
    async fn list_sandbox_tool_call_candidates(
        &self,
        _limit: u64,
    ) -> Result<Vec<crate::model::SandboxToolCall>> {
        agent_run_storage_unavailable()
    }

    /// List bounded oldest-first candidates for one exact immutable sandbox
    /// tool name. The matching claim method remains the ownership authority.
    async fn list_sandbox_tool_call_candidates_named(
        &self,
        _name: &str,
        _limit: u64,
    ) -> Result<Vec<crate::model::SandboxToolCall>> {
        agent_run_storage_unavailable()
    }

    /// Request cancellation using the database clock. Queued, waiting, and
    /// retry-wait runs become terminal immediately; a running worker retains
    /// its exact lease in `cancelling` until it acknowledges quiescence.
    async fn request_agent_run_cancellation(
        &self,
        _id: AgentRunId,
    ) -> Result<Option<RequestAgentRunCancellationOutcome>> {
        agent_run_storage_unavailable()
    }

    /// Fetch the exact immutable worker identity retained by a cancellation
    /// request. Trusted runtimes use this only for best-effort local wakeups;
    /// the cancellation row and run state remain authoritative.
    async fn get_agent_run_cancellation_signal(
        &self,
        _id: AgentRunId,
    ) -> Result<Option<crate::model::AgentRunCancellationSignal>> {
        agent_run_storage_unavailable()
    }

    /// Acknowledge cancellation with one exact live sandbox lease.
    async fn finish_agent_run_cancellation(
        &self,
        _id: AgentRunId,
        _lease_token: uuid::Uuid,
    ) -> Result<Option<FinishAgentRunCancellationOutcome>> {
        agent_run_storage_unavailable()
    }

    /// Atomically persist immutable final text and complete one exact live
    /// sandbox lease. An exact ambiguous retry returns the original receipt;
    /// stale, cancelled, or differently-payloaded submissions return `None`.
    async fn submit_agent_run_result(
        &self,
        _id: AgentRunId,
        _lease_token: uuid::Uuid,
        _text: &str,
    ) -> Result<Option<SubmitAgentRunResultOutcome>> {
        agent_run_storage_unavailable()
    }

    /// Atomically submit a background run's own files as its terminal receipt.
    ///
    /// The outputs already exist: the run wrote them and the host published
    /// them by filename. Submission records which of them the run offers as its
    /// deliverables, so nothing here creates or renames conversation content.
    async fn submit_agent_run_submission(
        &self,
        _id: AgentRunId,
        _lease_token: uuid::Uuid,
        _outputs: &[crate::AgentRunSubmittedOutput],
        _summary: &str,
    ) -> Result<Option<SubmitAgentRunResultOutcome>> {
        agent_run_storage_unavailable()
    }

    /// Atomically submit one validated folder-consent proposal as a sandbox's
    /// typed terminal receipt. This only wakes the foreground parent through
    /// its durable inbox; it cannot grant host access or invoke a client tool.
    async fn submit_agent_run_folder_access_proposal(
        &self,
        _id: AgentRunId,
        _lease_token: uuid::Uuid,
        _request: &crate::RequestFolderAccessArgs,
    ) -> Result<Option<SubmitAgentRunResultOutcome>> {
        agent_run_storage_unavailable()
    }

    /// Fence one exact sandbox lease after an execution failure. Attempts below
    /// the run budget become replay-safe retry work; the final attempt writes a
    /// parent-visible terminal receipt in the same transaction as `failed`.
    async fn fail_agent_run(
        &self,
        _id: AgentRunId,
        _lease_token: uuid::Uuid,
        _error_code: &str,
        _error_detail: &str,
        _retry_delay: chrono::Duration,
    ) -> Result<Option<FailAgentRunOutcome>> {
        agent_run_storage_unavailable()
    }

    /// List immutable child results delivered to one foreground coordinator.
    /// Consuming or waking a parent continuation is intentionally a separate
    /// state-machine transition.
    async fn list_agent_run_inbox(
        &self,
        _parent_run_id: AgentRunId,
    ) -> Result<Vec<AgentRunInboxEntry>> {
        agent_run_storage_unavailable()
    }

    /// List a bounded set of ordered child waits for which every immutable
    /// result appears ready. This scan is advisory and never claims member
    /// inboxes; the exact wait-set resume transition remains authoritative.
    async fn list_ready_agent_run_wait_set_candidates(
        &self,
        _limit: u64,
    ) -> Result<Vec<AgentRunWaitSetCandidate>> {
        turn_storage_unavailable()
    }

    /// Fetch one durable turn by its exact idempotency identity.
    async fn get_turn_run(&self, _id: TurnId) -> Result<Option<TurnRun>> {
        turn_storage_unavailable()
    }

    /// List a chat's durable turn history in deterministic creation-time order.
    async fn list_turn_runs(&self, _chat_id: ChatId) -> Result<Vec<TurnRun>> {
        turn_storage_unavailable()
    }

    /// Count in-flight work across every chat: non-terminal turns plus live
    /// background-tier agent runs. See [`ActiveWorkSnapshot`] for what counts
    /// and why the definition is strict. Callers gating a host restart must
    /// treat an error as "not quiescent".
    async fn count_active_work(&self) -> Result<ActiveWorkSnapshot> {
        turn_storage_unavailable()
    }

    /// Atomically persist a user's initial message and queue its exact turn.
    ///
    /// `id` is a non-nil caller-visible idempotency identity. Repeating the same id,
    /// chat, model, and byte-exact content returns [`AcceptTurnOutcome::Existing`]
    /// without another message or turn. Reusing an id with a different chat,
    /// model, or byte-exact content returns
    /// [`AcceptTurnOutcome::IdentityConflict`]. A different live turn for the
    /// chat returns [`AcceptTurnOutcome::ChatBusy`].
    async fn accept_turn(
        &self,
        id: TurnId,
        chat_id: ChatId,
        model: &str,
        content: &str,
    ) -> Result<AcceptTurnOutcome> {
        self.accept_turn_with_attachments(id, chat_id, model, content, &[], &[], &[])
            .await
    }

    /// Accept a turn whose input message also carries image or file attachments.
    ///
    /// The attachments commit in the same transaction as the message and turn,
    /// and they participate in the same idempotency proof: a retry with the same
    /// id but different attachments is an [`AcceptTurnOutcome::IdentityConflict`],
    /// not a silent acceptance of the first submission. Each attachment is
    /// recorded at its position in its media-specific list, which is the order
    /// a reloaded transcript replays it in.
    ///
    /// `invoked_skills` names the skills the user explicitly asked this turn to
    /// use. It is accepted input like the model and the attachments, so it is
    /// part of the same idempotency proof: a retry naming different skills is a
    /// conflict rather than a silent acceptance of the first submission's list.
    /// Whether each name is a live skill is the caller's decision; storage only
    /// bounds the list and the shape of each name.
    ///
    /// Recording an attachment makes its blob live: any queued retirement for
    /// that blob is cancelled in the same transaction. Because blob ids are
    /// content-derived, re-submitting identical bytes re-references the existing
    /// blob rather than storing a second copy.
    #[allow(clippy::too_many_arguments)]
    async fn accept_turn_with_attachments(
        &self,
        _id: TurnId,
        _chat_id: ChatId,
        _model: &str,
        _content: &str,
        _images: &[ImageRef],
        _documents: &[DocumentId],
        _invoked_skills: &[String],
    ) -> Result<AcceptTurnOutcome> {
        turn_storage_unavailable()
    }

    /// Accept a turn with all renderer-hidden, message-scoped context needed to
    /// build its durable model projection.
    ///
    /// `voice_input_used` is kept separate from the caller-visible content so
    /// storage can derive the canonical note rather than accepting arbitrary
    /// model-only text over the wire. Implementations that have not added voice
    /// metadata can still serve the ordinary false case through the attachment
    /// method above.
    #[allow(clippy::too_many_arguments)]
    async fn accept_turn_with_message_context(
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
        if voice_input_used {
            return turn_storage_unavailable();
        }
        self.accept_turn_with_attachments(
            id,
            chat_id,
            model,
            content,
            images,
            documents,
            invoked_skills,
        )
        .await
    }

    /// Perform one durable claim action under a fresh exact lease.
    ///
    /// `lease_token` is the caller's idempotency identity: retrying it while its
    /// lease remains live returns the same running turn. Callers must retain it
    /// across an ambiguous commit and use a fresh token for a new claim attempt.
    /// Every successful claim increments `claim_count` and moves the turn to
    /// `running`. Queued, retry-wait, and expired-running claims also increment
    /// `attempt_count`; resuming claims retain the current failure attempt.
    /// Expired work is reclaimed only while another attempt is permitted. An
    /// expired cancellation or final attempt is terminalized with its exact
    /// routed journal event and returned instead of claiming another turn; the
    /// caller publishes it before scanning again. `lease_expires_at` must be
    /// after `now`.
    async fn claim_turn_run(
        &self,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
        _lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ClaimTurnRunOutcome> {
        turn_storage_unavailable()
    }

    /// Extend one exact live turn lease monotonically.
    ///
    /// Returns `false` if the turn is not running, the token differs, the lease
    /// already expired, or the proposed expiry does not extend the current one.
    async fn heartbeat_turn_run(
        &self,
        _id: TurnId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
        _lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        turn_storage_unavailable()
    }

    /// Report whether `lease_token` still owns the exact live segment of a turn.
    ///
    /// Returns [`TurnLeaseFence::Current`] only while the turn is running or
    /// cancelling under this exact token, its claim receipt still matches the
    /// turn's attempt and claim counters, and the lease has not expired at
    /// `now`. Any other state — a superseding claim, an expired lease, or a
    /// terminal turn — is [`TurnLeaseFence::Stale`]. This is a read-only fence a
    /// worker consults before committing an intermediate tool or message effect;
    /// it never mutates durable state.
    async fn fence_turn_lease(
        &self,
        _id: TurnId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<TurnLeaseFence> {
        turn_storage_unavailable()
    }

    /// Atomically accept one idempotent steering instruction for a live turn.
    ///
    /// The non-nil caller-supplied `id` also names the eventual user message.
    /// Exact retries compare chat, turn, byte-exact content, and interrupt intent.
    /// Queued, running, resuming, and retry-wait turns accept instructions;
    /// cancelling or terminal turns return
    /// [`AcceptTurnSteerOutcome::TurnUnavailable`].
    async fn accept_turn_steer(
        &self,
        _id: TurnSteerId,
        _turn_id: TurnId,
        _chat_id: ChatId,
        _content: &str,
        _interrupt: bool,
    ) -> Result<AcceptTurnSteerOutcome> {
        turn_storage_unavailable()
    }

    /// Accept a steer with renderer-hidden, message-scoped context needed for
    /// its eventual durable model projection.
    ///
    /// `voice_input_used` is kept separate from caller-visible content so the
    /// store derives the canonical model note. Implementations that do not yet
    /// persist this metadata can continue serving the ordinary false case.
    #[allow(clippy::too_many_arguments)]
    async fn accept_turn_steer_with_message_context(
        &self,
        id: TurnSteerId,
        turn_id: TurnId,
        chat_id: ChatId,
        content: &str,
        interrupt: bool,
        voice_input_used: bool,
    ) -> Result<AcceptTurnSteerOutcome> {
        if voice_input_used {
            return turn_storage_unavailable();
        }
        self.accept_turn_steer(id, turn_id, chat_id, content, interrupt)
            .await
    }

    /// List pending instructions only while the caller owns the exact live lease.
    ///
    /// `Some` is ordered by durable acceptance time then identity. `None` means
    /// the lease is stale, expired, cancelling, or otherwise no longer running.
    async fn list_pending_turn_steers(
        &self,
        _turn_id: TurnId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<Vec<TurnSteer>>> {
        turn_storage_unavailable()
    }

    /// Persist one pending steer as a user message under the exact live lease.
    ///
    /// An optional preceding assistant candidate, the steer message, the
    /// application receipt, the revision increment, and its [`AgentEvent::UserSteered`]
    /// journal row commit atomically in transcript order. The event ordinal is
    /// the worker's exact attempt identity. Exact retries by the same lease and
    /// ordinal return [`ApplyTurnSteerOutcome::Existing`] with the same journal
    /// row even after the turn advances. A stale lease, rejected steer, or
    /// different winning lease returns `None`.
    #[allow(clippy::too_many_arguments)]
    async fn apply_turn_steer(
        &self,
        _turn_id: TurnId,
        _lease_token: uuid::Uuid,
        _steer_id: TurnSteerId,
        _attempt_event_ordinal: i32,
        _preceding_assistant: Option<&Message>,
        _preceding_citations: &[crate::AssistantCitationInput],
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<JournaledTurnSteerOutcome>> {
        turn_storage_unavailable()
    }

    /// Atomically persist the final assistant message and complete its turn.
    ///
    /// The exact claim must still be live at the fresh operational `now`, and
    /// the output cannot be dated after it. Repeating the same token and
    /// exact output identity, content, and database-normalized timestamp after
    /// an ambiguous commit returns the completed turn even after lease expiry,
    /// without inserting another message. Returns
    /// `None` when the token never owned this turn, its lease was lost, or
    /// another terminal outcome already won. Pending steering and stale model
    /// output return explicit nonterminal outcomes so callers can continue the
    /// same live attempt rather than mistaking them for lease loss. The caller
    /// must pass the `steer_revision` captured before generation;
    /// completion is fenced if another steer was applied in the meantime.
    async fn complete_turn_run(
        &self,
        _id: TurnId,
        _lease_token: uuid::Uuid,
        _expected_steer_revision: i64,
        _now: chrono::DateTime<chrono::Utc>,
        _output: &Message,
    ) -> Result<Option<CompleteTurnRunOutcome>> {
        turn_storage_unavailable()
    }

    /// Complete one claimed turn and append its terminal event atomically.
    ///
    /// Exact ambiguous retries recover both the completed turn and the same
    /// journal sequence. No terminal event is visible unless the output message
    /// and terminal state transition commit with it.
    #[allow(clippy::too_many_arguments)]
    async fn complete_turn_run_and_append_event(
        &self,
        _id: TurnId,
        _lease_token: uuid::Uuid,
        _expected_steer_revision: i64,
        _now: chrono::DateTime<chrono::Utc>,
        _output: &Message,
        _usage: Usage,
        _stop_reason: StopReason,
    ) -> Result<Option<JournaledTurnOutcome<CompleteTurnRunOutcome>>> {
        turn_storage_unavailable()
    }

    /// Complete one claimed turn with ordered evidence-backed assistant sources.
    ///
    /// The clean message, resolved same-turn citations, terminal transition, and
    /// journal event commit together. Unknown opaque references are ignored.
    #[allow(clippy::too_many_arguments)]
    async fn complete_turn_run_with_citations_and_append_event(
        &self,
        id: TurnId,
        lease_token: uuid::Uuid,
        expected_steer_revision: i64,
        now: chrono::DateTime<chrono::Utc>,
        output: &Message,
        citations: &[crate::AssistantCitationInput],
        usage: Usage,
        stop_reason: StopReason,
    ) -> Result<Option<JournaledTurnOutcome<CompleteTurnRunOutcome>>> {
        if citations.is_empty() {
            self.complete_turn_run_and_append_event(
                id,
                lease_token,
                expected_steer_revision,
                now,
                output,
                usage,
                stop_reason,
            )
            .await
        } else {
            turn_storage_unavailable()
        }
    }

    /// Complete one claimed turn as a refusal and append that structured
    /// terminal event atomically with its partial-or-empty assistant output.
    #[allow(clippy::too_many_arguments)]
    async fn complete_refused_turn_run_with_citations_and_append_event(
        &self,
        _id: TurnId,
        _lease_token: uuid::Uuid,
        _expected_steer_revision: i64,
        _now: chrono::DateTime<chrono::Utc>,
        _output: &Message,
        _citations: &[crate::AssistantCitationInput],
        _usage: Usage,
        _refusal: RefusalOutcome,
    ) -> Result<Option<JournaledTurnOutcome<CompleteTurnRunOutcome>>> {
        turn_storage_unavailable()
    }

    /// Atomically record a failure for one exact live claimed attempt.
    ///
    /// `now` is a fresh operational lease fence and is not part of the stable
    /// request identity. An exact retry is identified by the turn, claim token,
    /// retry intent, cumulative model steps and usage, error code, and error
    /// detail; it returns `Existing` even if a later attempt has already advanced
    /// the mutable turn. Reusing a token with different request data is an error.
    /// A requested retry moves the turn to `retry_wait` only while attempts
    /// remain; otherwise the result is terminally `failed`. Returns `None` when
    /// this claim did not win the live attempt or another resolution already did.
    #[allow(clippy::too_many_arguments)]
    async fn record_turn_run_failure(
        &self,
        _id: TurnId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
        _retry: TurnFailureRetry,
        _model_steps: i32,
        _usage: Usage,
        _error_code: &str,
        _error_detail: Option<&str>,
    ) -> Result<Option<RecordTurnFailureOutcome>> {
        turn_storage_unavailable()
    }

    /// Resolve one claimed failure and append its terminal event atomically.
    ///
    /// Retry-wait outcomes do not publish a terminal event. Terminal failures
    /// commit their receipt, turn transition, and `TurnFailed` journal row in
    /// one transaction, and exact ambiguous retries recover the original
    /// journal sequence.
    #[allow(clippy::too_many_arguments)]
    async fn record_turn_run_failure_and_append_event(
        &self,
        _id: TurnId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
        _retry: TurnFailureRetry,
        _model_steps: i32,
        _usage: Usage,
        _error_code: &str,
        _error_detail: Option<&str>,
    ) -> Result<Option<JournaledTurnOutcome<RecordTurnFailureOutcome>>> {
        turn_storage_unavailable()
    }

    /// Durably request cancellation for one exact turn.
    ///
    /// Queued, retry-wait, and resuming work becomes terminal immediately.
    /// Running work enters `cancelling` while retaining its exact lease, so the
    /// database's one-live-turn-per-chat invariant remains held until the
    /// cooperative worker actually stops. The empty-payload request converges
    /// on the exact turn identity, so cancelling/cancelled retries return
    /// `Existing`.
    async fn request_turn_cancellation(
        &self,
        _id: TurnId,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<RequestTurnCancellationOutcome>> {
        turn_storage_unavailable()
    }

    /// Request cancellation and publish an immediate terminal outcome atomically.
    ///
    /// Queued, retry-wait, and resuming turns commit `TurnCancelled` with their
    /// terminal transition. Running turns only enter `cancelling`; their worker
    /// publishes the terminal event when it acknowledges quiescence.
    async fn request_turn_cancellation_and_append_event(
        &self,
        _id: TurnId,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<JournaledTurnOutcome<RequestTurnCancellationOutcome>>> {
        turn_storage_unavailable()
    }

    /// Acknowledge that one exact cancelling worker has quiesced.
    ///
    /// The immutable claim receipt and terminal attempt make exact retries
    /// recoverable after lease expiry. Returns `None` for a stale token, a turn
    /// that is not cancelling, or a first-time acknowledgement with regressing
    /// operational time.
    async fn finish_turn_cancellation(
        &self,
        _id: TurnId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<FinishTurnCancellationOutcome>> {
        turn_storage_unavailable()
    }

    /// Acknowledge cancellation and publish its terminal event atomically.
    ///
    /// Exact ambiguous retries recover both the cancelled turn and the same
    /// journal sequence, including the usage recorded by the original worker.
    ///
    /// `output` carries the prose the cancelled turn had already streamed; a
    /// non-empty output commits as the turn's durable assistant message in the
    /// same transaction, so reload and the next model turn keep what the user
    /// was reading when they stopped the run (#1182).
    async fn finish_turn_cancellation_and_append_event(
        &self,
        _id: TurnId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
        _usage: Usage,
        _output: Option<&Message>,
        _citations: &[crate::AssistantCitationInput],
    ) -> Result<Option<JournaledTurnOutcome<FinishTurnCancellationOutcome>>> {
        turn_storage_unavailable()
    }

    /// Atomically persist one client-executed tool call, record the exact
    /// originating worker claim, and release the turn lease.
    ///
    /// Exact retries recover through the immutable wait receipt even after the
    /// client call resolves or the turn advances. The exact progress delta is
    /// part of that retry identity and is folded into turn-wide checkpoint
    /// accounting at most once. A pending steer fences the checkpoint so the
    /// worker can apply that instruction first.
    async fn park_turn_for_client_tool_call(
        &self,
        _turn_id: TurnId,
        _lease_token: uuid::Uuid,
        _expected_steer_revision: i64,
        _progress: TurnCheckpointProgress,
        _now: chrono::DateTime<chrono::Utc>,
        _call: &ClientToolCallRequest,
    ) -> Result<Option<ParkTurnForClientCallOutcome>> {
        turn_storage_unavailable()
    }

    /// Persist an ordered, unique, bounded child set and release a claimed
    /// foreground turn in the same transaction. Every child must carry an
    /// immutable sandbox admission owned by this exact origin turn. Exact
    /// retries recover the receipt before lease expiry or steering state is
    /// considered.
    async fn park_turn_for_agent_run_wait_set(
        &self,
        _request: &crate::model::AgentRunWaitSetCheckpointRequest,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<ParkTurnForAgentRunWaitSetOutcome>> {
        turn_storage_unavailable()
    }

    /// Consume every matching child inbox result exactly once and wake the
    /// foreground turn when the committed completion condition is satisfied.
    /// Results are returned in immutable request order, never delivery order.
    /// An exact retry with `resume_token` recovers the prior transition before
    /// mutable parent liveness is checked.
    async fn resume_turn_for_agent_run_wait_set(
        &self,
        _wait_id: CallId,
        _resume_token: uuid::Uuid,
    ) -> Result<Option<ResumeTurnForAgentRunWaitSetOutcome>> {
        turn_storage_unavailable()
    }

    /// Append a message to its chat.
    async fn append_message(&self, message: &Message) -> Result<()>;

    /// Atomically append a clean assistant message and its exact evidence-backed sources.
    async fn append_assistant_message_with_citations(
        &self,
        message: &Message,
        references: &[crate::AssistantCitationInput],
    ) -> Result<()> {
        if references.is_empty() {
            self.append_message(message).await
        } else {
            Err(AgentError::Store(
                "assistant citation storage is not implemented by this Store".into(),
            ))
        }
    }

    /// Atomically append one intermediate assistant message and its citations
    /// only while `lease_token` owns the exact live turn segment.
    async fn append_claimed_assistant_message_with_citations(
        &self,
        _message: &Message,
        _references: &[crate::AssistantCitationInput],
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<AppendClaimedMessageOutcome> {
        turn_storage_unavailable()
    }

    /// List a chat's messages in creation order.
    async fn list_messages(&self, chat_id: ChatId) -> Result<Vec<Message>>;

    /// Output message ids of the chat's cancelled turns (#1182).
    ///
    /// Context assembly appends an interruption note to these messages so the
    /// model is told the user stopped the response there, rather than left to
    /// infer it from a mid-sentence cut. Best-effort: the default keeps stores
    /// without turn state serving unannotated transcripts.
    async fn list_cancelled_output_message_ids(&self, _chat_id: ChatId) -> Result<Vec<MessageId>> {
        Ok(Vec::new())
    }

    /// List a chat's image attachments, ordered by message then position.
    ///
    /// The block transcript is rebuilt on load rather than stored, so this is
    /// how history regains the images a turn was submitted with. Stores without
    /// attachment support report none, which degrades a reloaded turn to its
    /// text rather than failing the load.
    async fn list_message_attachments(&self, _chat_id: ChatId) -> Result<Vec<MessageAttachment>> {
        Ok(Vec::new())
    }

    /// List a chat's file attachments, ordered by message then position.
    async fn list_message_document_attachments(
        &self,
        _chat_id: ChatId,
    ) -> Result<Vec<MessageDocumentAttachment>> {
        Ok(Vec::new())
    }

    /// Accept immutable canonical tool-call identity and arguments exactly once.
    async fn accept_tool_call(&self, call: &ToolCallRecord) -> Result<AcceptToolCallOutcome>;

    /// Atomically accept one server tool call only while its exact originating
    /// turn lease remains live. The stored lease is private replay state.
    async fn accept_claimed_tool_call(
        &self,
        _call: &ToolCallRecord,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<AcceptClaimedToolCallOutcome> {
        turn_storage_unavailable()
    }

    /// Register a Sensitive server tool call for durable human review.
    async fn request_tool_call_approval(
        &self,
        _request: &ApprovalRequest,
        _requested_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<RequestToolApprovalOutcome> {
        Err(AgentError::Store(
            "durable tool approval storage is not implemented by this Store".into(),
        ))
    }

    /// Register an approval and append `ApprovalRequired` in one claimed-turn
    /// transaction. Exact retries recover the same event sequence.
    async fn request_tool_call_approval_and_append_event(
        &self,
        _request: &ApprovalRequest,
        _lease_token: uuid::Uuid,
        _event_ordinal: i32,
        _requested_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<JournaledToolApprovalOutcome> {
        Err(AgentError::Store(
            "journaled durable tool approval storage is not implemented by this Store".into(),
        ))
    }

    /// Decide a previously registered approval exactly once.
    async fn decide_tool_call_approval(
        &self,
        _chat_id: ChatId,
        _call_id: CallId,
        _decision: &ApprovalDecision,
        _decided_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<DecideToolApprovalOutcome> {
        Err(AgentError::Store(
            "durable tool approval storage is not implemented by this Store".into(),
        ))
    }

    /// Decide a pending approval and persist its chosen chat-scoped standing
    /// grant in the same transaction. A grant can only be added while this
    /// exact call is pending; a later retry may not widen a one-shot decision.
    async fn decide_tool_call_approval_with_grant(
        &self,
        _chat_id: ChatId,
        _call_id: CallId,
        _decision: &ApprovalDecision,
        _grant: &StandingGrant,
        _decided_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<DecideToolApprovalOutcome> {
        Err(AgentError::Store(
            "durable standing-grant storage is not implemented by this Store".into(),
        ))
    }

    /// Read private approval state for exact recovery.
    async fn get_tool_call_approval(&self, _call_id: CallId) -> Result<Option<ToolApproval>> {
        Err(AgentError::Store(
            "durable tool approval storage is not implemented by this Store".into(),
        ))
    }

    /// A bounded page of calls the Auto-mode judge currently owns, oldest
    /// first, across all chats.
    async fn list_judging_tool_call_approvals(&self, _limit: u64) -> Result<Vec<ToolApproval>> {
        Err(AgentError::Store(
            "durable tool approval storage is not implemented by this Store".into(),
        ))
    }

    /// Land the Auto-mode judge's verdict on one parked call. `false` means
    /// the judge no longer owns it (a human got there first, or it resolved).
    async fn resolve_tool_call_approval_from_judge(
        &self,
        _chat_id: ChatId,
        _call_id: CallId,
        _approved: bool,
    ) -> Result<bool> {
        Err(AgentError::Store(
            "durable tool approval storage is not implemented by this Store".into(),
        ))
    }

    /// Every durable standing grant, newest first, across all chats.
    ///
    /// A malformed row is skipped, never surfaced: what cannot be described
    /// cannot be knowingly kept, and it already fails to authorize anything
    /// at match time for the same reason.
    async fn list_standing_tool_grants(&self) -> Result<Vec<crate::approval::StandingGrantRecord>> {
        Err(AgentError::Store(
            "durable standing-grant storage is not implemented by this Store".into(),
        ))
    }

    /// Withdraw one standing grant by the approval that created it. Later
    /// matching calls park on the gate again. Returns `false` when no such
    /// grant exists (already revoked, or never granted).
    async fn revoke_standing_tool_grant(&self, _source_call_id: CallId) -> Result<bool> {
        Err(AgentError::Store(
            "durable standing-grant storage is not implemented by this Store".into(),
        ))
    }

    /// List a bounded page of pending approvals for one chat.
    async fn list_pending_tool_call_approvals(
        &self,
        _chat_id: ChatId,
        _limit: u64,
    ) -> Result<Vec<ToolApproval>> {
        Err(AgentError::Store(
            "durable tool approval storage is not implemented by this Store".into(),
        ))
    }

    /// Claim the first lease with a caller-generated secret fencing token.
    /// A retry with the same executor and token recovers the original live
    /// claim even when the caller proposes a newly calculated expiry.
    async fn claim_client_tool_call(
        &self,
        id: CallId,
        chat_id: ChatId,
        executor_id: uuid::Uuid,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ClaimClientToolCallOutcome>;

    /// Monotonically extend an exact live client-execution lease.
    async fn heartbeat_client_tool_call(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<HeartbeatClientToolCallOutcome>;

    /// Resolve a pending server-executed tool call exactly once.
    async fn resolve_server_tool_call(
        &self,
        id: CallId,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ResolveToolCallOutcome>;

    /// Resolve a server call and retain the renderer projection it produced.
    async fn resolve_server_tool_call_with_artifacts(
        &self,
        id: CallId,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
        _preview: Option<&crate::ToolResultPreview>,
    ) -> Result<ResolveToolCallOutcome> {
        self.resolve_server_tool_call(id, resolution, resolved_at)
            .await
    }

    /// Resolve a server tool result only if the same live turn lease that
    /// accepted the call still owns the turn.
    #[allow(clippy::too_many_arguments)]
    async fn resolve_claimed_server_tool_call(
        &self,
        _id: CallId,
        _chat_id: ChatId,
        _turn_id: TurnId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
        _resolution: &ToolCallResolution,
        _resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ResolveToolCallOutcome> {
        turn_storage_unavailable()
    }

    /// The claimed-lease counterpart of
    /// [`Self::resolve_server_tool_call_with_artifacts`].
    #[allow(clippy::too_many_arguments)]
    async fn resolve_claimed_server_tool_call_with_artifacts(
        &self,
        id: CallId,
        chat_id: ChatId,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
        _preview: Option<&crate::ToolResultPreview>,
    ) -> Result<ResolveToolCallOutcome> {
        self.resolve_claimed_server_tool_call(
            id,
            chat_id,
            turn_id,
            lease_token,
            now,
            resolution,
            resolved_at,
        )
        .await
    }

    /// Resolve a pending server call recovered at worker startup without
    /// executing it again. An exact live lease for the same turn may commit
    /// this conservative interrupted result, including after a process restart
    /// that retained the lease.
    #[allow(clippy::too_many_arguments)]
    async fn abandon_inherited_server_tool_call(
        &self,
        _id: CallId,
        _chat_id: ChatId,
        _turn_id: TurnId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
        _resolution: &ToolCallResolution,
        _resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ResolveToolCallOutcome> {
        turn_storage_unavailable()
    }

    /// Resolve a pending client call under its exact unexpired executor lease.
    /// Once committed, the token and terminal payload are the stable retry
    /// identity; `resolved_at` records the first commit and is not compared on
    /// an ambiguous retry.
    async fn resolve_client_tool_call(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ResolveToolCallOutcome> {
        Ok(self
            .resolve_client_tool_call_and_append_event(
                id,
                chat_id,
                lease_token,
                now,
                resolution,
                resolved_at,
            )
            .await?
            .outcome)
    }

    /// Resolve a live client call and return any atomic turn transition receipt.
    /// Exact retries recover the same terminal event when client-owned
    /// cancellation completed with this resolution.
    async fn resolve_client_tool_call_and_append_event(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<JournaledClientToolCallOutcome>;

    /// Resolve a live client call, retaining the rows it reported.
    ///
    /// `rows` is the executor's *unvalidated* `{entries, failures}` payload, not
    /// a projection. The store builds the projection from it against the call's
    /// own stored name, so the allowlist and every clamp are applied here rather
    /// than trusted from the executor — a client cannot award itself a card for
    /// a tool that has none, nor an unbounded row.
    ///
    /// The default drops the rows, which costs the card and nothing else.
    #[allow(clippy::too_many_arguments)]
    async fn resolve_client_tool_call_and_append_event_with_rows(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
        _rows: Option<&serde_json::Value>,
    ) -> Result<JournaledClientToolCallOutcome> {
        self.resolve_client_tool_call_and_append_event(
            id,
            chat_id,
            lease_token,
            now,
            resolution,
            resolved_at,
        )
        .await
    }

    /// Resolve a known outcome after the exact client lease expired.
    ///
    /// This is the explicit recovery path for an ambiguous native interaction;
    /// it never transfers the call to another executor.
    async fn resolve_expired_client_tool_call(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ResolveToolCallOutcome> {
        Ok(self
            .resolve_expired_client_tool_call_and_append_event(
                id,
                chat_id,
                lease_token,
                now,
                resolution,
                resolved_at,
            )
            .await?
            .outcome)
    }

    /// Reconcile an expired client call and return any atomic turn transition
    /// receipt, with the same retry behavior as the live resolution path.
    async fn resolve_expired_client_tool_call_and_append_event(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<JournaledClientToolCallOutcome>;

    /// The expired-lease counterpart of
    /// [`Self::resolve_client_tool_call_and_append_event_with_rows`].
    #[allow(clippy::too_many_arguments)]
    async fn resolve_expired_client_tool_call_and_append_event_with_rows(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
        _rows: Option<&serde_json::Value>,
    ) -> Result<JournaledClientToolCallOutcome> {
        self.resolve_expired_client_tool_call_and_append_event(
            id,
            chat_id,
            lease_token,
            now,
            resolution,
            resolved_at,
        )
        .await
    }

    /// List unclaimed and claimed client work for authoritative recovery.
    async fn list_pending_client_tool_calls(&self, chat_id: ChatId) -> Result<Vec<ToolCallRecord>>;

    /// List only validated renderer-safe foreground question cards.
    async fn list_pending_user_questions(
        &self,
        _chat_id: ChatId,
    ) -> Result<Vec<PendingUserQuestions>> {
        turn_storage_unavailable()
    }

    /// List every conversation that has a renderer-owned prompt awaiting the
    /// user. The result carries opaque call ids only; callers fetch detail for
    /// an individual open conversation through its dedicated recovery route.
    async fn list_pending_chat_prompts(&self) -> Result<Vec<PendingChatPrompt>> {
        turn_storage_unavailable()
    }

    /// Every item waiting on `owner`, across their conversations, oldest first.
    ///
    /// A read model over the same parked rows the per-chat recovery routes
    /// serve: an item disappears from here exactly when its own resolution
    /// path lands, because there is nothing else to update.
    async fn list_inbox_items_scoped(&self, _owner: &OwnerId) -> Result<Vec<InboxItem>> {
        turn_storage_unavailable()
    }

    /// Atomically commit exact answers, complete the same tool call, and move
    /// its blocked turn to the shared resumable state.
    async fn answer_user_questions(
        &self,
        _request: &AnswerUserQuestionsRequest,
        _answered_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<AnswerUserQuestionsOutcome> {
        turn_storage_unavailable()
    }

    /// List only validated renderer-safe pending plan proposals.
    async fn list_pending_plan_approvals(
        &self,
        _chat_id: ChatId,
    ) -> Result<Vec<crate::PendingPlanApproval>> {
        turn_storage_unavailable()
    }

    /// Atomically commit one exact plan decision, complete the same tool
    /// call, move its blocked turn to the shared resumable state, and — on
    /// accept — move the chat out of plan mode.
    async fn decide_plan(
        &self,
        _request: &crate::DecidePlanRequest,
        _decided_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<DecidePlanOutcome> {
        turn_storage_unavailable()
    }

    /// Replace a chat's task plan and journal the bounded refresh hint.
    ///
    /// `call_id` names the already-admitted `update_task_plan` call, which is
    /// what scopes the write to a turn and its lease. Steps must already be
    /// validated at the tool boundary; storage records what it is given.
    ///
    /// `Ok(None)` means the write was declined because the call's attempt no
    /// longer owns its turn — an ordinary retry outcome, not a failure.
    async fn update_task_plan(
        &self,
        _chat_id: ChatId,
        _call_id: CallId,
        _steps: &[crate::TaskPlanStep],
        _updated_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<crate::TaskPlan>> {
        turn_storage_unavailable()
    }

    /// The chat's current task plan, or `None` when it has never made one.
    async fn get_task_plan(&self, _chat_id: ChatId) -> Result<Option<crate::TaskPlan>> {
        turn_storage_unavailable()
    }

    /// List a chat's tool calls in creation order.
    async fn list_tool_calls(&self, chat_id: ChatId) -> Result<Vec<ToolCallRecord>>;

    /// Read a setting (profile, model prefs, approval policy), or `None`.
    async fn get_setting(&self, key: &str) -> Result<Option<Value>>;

    /// Write a setting.
    async fn set_setting(&self, key: &str, value: &Value) -> Result<()>;

    /// Append an event for the legacy direct-execution path.
    ///
    /// Sequence numbers are per-chat and monotonic (starting at 1). This method
    /// rejects a chat once it has any durable turn history; durable workers must
    /// use [`append_turn_event`](Self::append_turn_event) so stale attempts are
    /// fenced and ambiguous retries recover the original sequence.
    async fn append_event(&self, chat_id: ChatId, event: &AgentEvent) -> Result<i64>;

    /// Append a nonterminal event owned by an exact live turn attempt.
    ///
    /// `(lease_token, attempt_event_ordinal)` is the idempotency identity. An
    /// exact retry returns the original sequence even after lease loss; reusing
    /// it with different data is an error. A first append succeeds only while
    /// the matching attempt still owns a live running lease. Completed, failed,
    /// and cancelled events are reserved for atomic turn resolution.
    async fn append_turn_event(
        &self,
        _chat_id: ChatId,
        _turn_id: TurnId,
        _lease_token: uuid::Uuid,
        _attempt_event_ordinal: i32,
        _now: chrono::DateTime<chrono::Utc>,
        _event: &AgentEvent,
    ) -> Result<Option<i64>> {
        turn_storage_unavailable()
    }

    /// Recover a terminal event only when it was committed by this exact lease
    /// with the byte-equivalent payload.
    ///
    /// This distinguishes an ambiguous response after this worker's commit from
    /// a claim scanner or competing terminal resolution that reached the same
    /// status with a different immutable receipt. Returns `None` for any
    /// different terminal identity.
    async fn recover_exact_turn_terminal_event(
        &self,
        _turn_id: TurnId,
        _lease_token: uuid::Uuid,
        _event: &AgentEvent,
    ) -> Result<Option<SequencedEvent>> {
        turn_storage_unavailable()
    }

    /// Recover a completed turn only when its output, ordered citations, and
    /// terminal event match the exact request whose response was ambiguous.
    ///
    /// Stores without structured citation support retain the legacy recovery
    /// path for citation-free outputs. Citation-aware stores must override this
    /// method so a matching message identity cannot conceal different sources.
    async fn recover_exact_completed_turn_event(
        &self,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        _output: &Message,
        citations: &[crate::AssistantCitationInput],
        event: &AgentEvent,
    ) -> Result<Option<SequencedEvent>> {
        if citations.is_empty() {
            self.recover_exact_turn_terminal_event(turn_id, lease_token, event)
                .await
        } else {
            turn_storage_unavailable()
        }
    }

    /// List a chat's journaled events with `seq` greater than `after`, in
    /// sequence order. Pass `0` to replay from the start.
    async fn list_events(&self, chat_id: ChatId, after: i64) -> Result<Vec<SequencedEvent>>;

    // --- Durable reverse-RPC operation log (issue #858) ---
    //
    // These back the crash-safe `OperationStore` seam of
    // `openwave-sandbox-protocol`. The store persists an opaque
    // `(fingerprint, body)` pair keyed by `(run_id, operation_id)` and enforces
    // the commit predicate transactionally; the protocol tier owns the typed
    // meaning of those bytes and the mapping to `ClaimOutcome`. Retention and
    // body eviction are #859; `evict_operation` is that seam.

    /// Atomically claim `operation_id` under `run_id` for `fingerprint`, or
    /// observe its existing state, in a single transaction.
    ///
    /// `owner_epoch` identifies the claiming process lifetime: a `Claimed` entry
    /// found under a *different* epoch is the after-crash ambiguity
    /// ([`OperationClaimOutcome::ForeignClaim`]) for an `external_effect`
    /// operation; under the *same* epoch it is a concurrent duplicate
    /// ([`OperationClaimOutcome::OwnedClaim`]). A foreign `Claimed` with no
    /// external effect is safe to re-drive, so ownership is taken over and the
    /// claim reported [`OperationClaimOutcome::Fresh`].
    async fn claim_operation(
        &self,
        _run_id: uuid::Uuid,
        _operation_id: uuid::Uuid,
        _fingerprint: &[u8],
        _external_effect: bool,
        _owner_epoch: uuid::Uuid,
    ) -> Result<OperationClaimOutcome> {
        operation_log_storage_unavailable()
    }

    /// Settle a `Claimed` entry to `Recorded` with `body`, transactionally.
    /// Idempotent: a re-delivered record for an already-`Recorded` entry is
    /// acknowledged ([`OperationLogWrite::AlreadyTerminal`]) without overwriting
    /// the first-committed body.
    async fn record_operation(
        &self,
        _run_id: uuid::Uuid,
        _operation_id: uuid::Uuid,
        _body: &[u8],
    ) -> Result<OperationLogWrite> {
        operation_log_storage_unavailable()
    }

    /// Settle a `Claimed` entry to `Failed` with `body`, transactionally.
    /// Idempotent for an already-`Failed` entry.
    async fn fail_operation(
        &self,
        _run_id: uuid::Uuid,
        _operation_id: uuid::Uuid,
        _body: &[u8],
    ) -> Result<OperationLogWrite> {
        operation_log_storage_unavailable()
    }

    /// The current state of an operation-log entry, if the log knows it.
    async fn operation_state(
        &self,
        _run_id: uuid::Uuid,
        _operation_id: uuid::Uuid,
    ) -> Result<Option<OperationLogEntry>> {
        operation_log_storage_unavailable()
    }

    /// Drop a terminal entry's replay body once the sandbox acknowledges
    /// consuming its response. The durable row remains as a commit marker so
    /// the operation identity can never execute again.
    async fn evict_operation(&self, _run_id: uuid::Uuid, _operation_id: uuid::Uuid) -> Result<()> {
        operation_log_storage_unavailable()
    }

    /// How many operation-log entries a run currently retains. For tests and,
    /// later, retention accounting.
    async fn operation_log_len(&self, _run_id: uuid::Uuid) -> Result<usize> {
        operation_log_storage_unavailable()
    }

    /// How many terminal operation-log bodies remain available for replay.
    /// Commit markers are excluded so this measures replay retention rather
    /// than audit/identity cardinality.
    async fn retained_operation_body_count(&self, _run_id: uuid::Uuid) -> Result<usize> {
        operation_log_storage_unavailable()
    }
}
