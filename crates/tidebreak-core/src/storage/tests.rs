use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use futures::executor::block_on;

use super::*;
mod root_attachment;

use async_trait::async_trait;
use serde_json::Value;

use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::{CallId, ChatId, DocumentId, ProjectId, RootAttachmentChangeId};
use crate::model::{
    validate_chat_root_projection, validate_chat_root_projection_against_project,
    validate_project_root_projection, ChatRootAttachment, RootAttachmentChangeAction,
    RootAttachmentChangeFailure, RootAttachmentChangePhase, RootAttachmentOrigin,
    RootAttachmentSubjectKind, ToolCallExecution, ToolCallStatus, MAX_ATTACHMENT_REVISION,
    MAX_ROOT_ATTACHMENTS,
};
use crate::model::{
    BeginRootAttachmentChange, Chat, DocumentListCursor, DocumentRecord, DocumentScope,
    DocumentSourceUpsert, DocumentSummaryRecord, DocumentUpsert, Message, Project, ReasoningEffort,
    RootAttachmentChange, RootAttachmentChangeTerminal, ToolCallRecord, ToolCallResolution,
};
use crate::PermissionMode;

/// Minimal in-memory `Store` — proves the trait is object-safe and usable
/// behind `Arc<dyn Store>`, and exercises the signatures.
#[derive(Default)]
struct MemDocumentState {
    documents: HashMap<DocumentId, DocumentRecord>,
}

#[derive(Default)]
struct MemStore {
    projects: Mutex<HashMap<ProjectId, Project>>,
    document_state: Mutex<MemDocumentState>,
    chats: Mutex<HashMap<ChatId, Chat>>,
    root_attachment_changes: Mutex<HashMap<RootAttachmentChangeId, RootAttachmentChange>>,
    settings: Mutex<HashMap<String, Value>>,
    events: Mutex<Vec<(ChatId, SequencedEvent)>>,
    tool_calls: Mutex<HashMap<crate::id::CallId, ToolCallRecord>>,
    tool_history_order: Mutex<HashMap<crate::id::CallId, (ChatId, i64)>>,
    tool_call_lease_tokens: Mutex<HashMap<crate::id::CallId, uuid::Uuid>>,
}

impl MemStore {
    fn resolve_mem_tool_call(
        &self,
        id: CallId,
        client_authority: Option<(ChatId, uuid::Uuid, chrono::DateTime<chrono::Utc>, bool)>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ResolveToolCallOutcome> {
        let mut calls = self.tool_calls.lock().unwrap();
        let Some(call) = calls.get_mut(&id) else {
            return Ok(ResolveToolCallOutcome::NotFound);
        };
        let stored_lease_token = self
            .tool_call_lease_tokens
            .lock()
            .unwrap()
            .get(&id)
            .copied();
        let (error_code, error_detail) = match resolution {
            ToolCallResolution::Failed {
                error_code,
                error_detail,
                ..
            } => (Some(error_code.clone()), error_detail.clone()),
            ToolCallResolution::Completed { .. } | ToolCallResolution::Cancelled { .. } => {
                (None, None)
            }
        };
        if call.status.is_terminal() {
            let authority_matches = match client_authority {
                None => call.execution == ToolCallExecution::Server && stored_lease_token.is_none(),
                Some((chat_id, lease_token, _, _)) => {
                    call.chat_id == chat_id
                        && call.execution == ToolCallExecution::Client
                        && stored_lease_token == Some(lease_token)
                }
            };
            if !authority_matches {
                return Ok(ResolveToolCallOutcome::LeaseLost);
            }
            let exact = call.status == resolution.status()
                && call.result.as_deref() == Some(resolution.result())
                && call.error_code == error_code
                && call.error_detail == error_detail;
            return Ok(if exact {
                ResolveToolCallOutcome::Existing
            } else {
                ResolveToolCallOutcome::AlreadyTerminal
            });
        }
        let owns = match client_authority {
            None => call.execution == ToolCallExecution::Server,
            Some((chat_id, lease_token, now, expired)) => {
                call.chat_id == chat_id
                    && call.execution == ToolCallExecution::Client
                    && stored_lease_token == Some(lease_token)
                    && call.client_lease_expires_at.is_some_and(|expiry| {
                        if expired {
                            expiry <= now
                        } else {
                            expiry > now
                        }
                    })
            }
        };
        if !owns {
            return Ok(ResolveToolCallOutcome::LeaseLost);
        }
        call.status = resolution.status();
        call.result = Some(resolution.result().to_owned());
        call.error_code = error_code;
        call.error_detail = error_detail;
        call.client_lease_expires_at = None;
        call.resolved_at = Some(resolved_at);
        Ok(ResolveToolCallOutcome::Resolved)
    }
}

fn root_attachment_terminal_matches(
    change: &RootAttachmentChange,
    terminal: &RootAttachmentChangeTerminal,
) -> bool {
    match terminal {
        RootAttachmentChangeTerminal::Completed {
            broker_changed,
            broker_currently_attached,
        } => {
            change.phase == RootAttachmentChangePhase::Completed
                && change.broker_changed == Some(*broker_changed)
                && change.broker_currently_attached == Some(*broker_currently_attached)
                && change.failure.is_none()
        }
        RootAttachmentChangeTerminal::Failed {
            broker_changed,
            broker_currently_attached,
            failure,
        } => {
            change.phase == RootAttachmentChangePhase::Failed
                && change.broker_changed == *broker_changed
                && change.broker_currently_attached == *broker_currently_attached
                && change.failure.as_ref() == Some(failure)
        }
    }
}

fn canonical_root_attachment_timestamp(
    timestamp: chrono::DateTime<chrono::Utc>,
) -> Result<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::from_timestamp_micros(timestamp.timestamp_micros())
        .ok_or_else(|| AgentError::Store("timestamp is outside the database range".into()))
}

fn remove_exact_attachment(chat: &mut Chat, change: &RootAttachmentChange) -> Result<()> {
    let position = change
        .projection_position
        .ok_or_else(|| AgentError::Store("root attachment change is missing its position".into()))?
        as usize;
    if chat
        .root_attachments
        .get(position)
        .is_none_or(|attachment| attachment.root_id != change.root_id)
    {
        return Err(AgentError::Store(
            "root attachment change projection no longer matches its intent".into(),
        ));
    }
    chat.root_attachments.remove(position);
    Ok(())
}

fn validate_mem_pending_attachment(chat: &Chat, change: &RootAttachmentChange) -> Result<()> {
    let found = chat
        .root_attachments
        .iter()
        .position(|attachment| attachment.root_id == change.root_id);
    let expected_present =
        change.projection_existed_before || change.action == RootAttachmentChangeAction::Attach;
    if found.is_some() != expected_present {
        return Err(AgentError::Store(
            "root attachment change pending projection is inconsistent".into(),
        ));
    }
    if let Some(position) = found {
        if change.projection_position.map(|position| position as usize) != Some(position)
            || change.origin != Some(chat.root_attachments[position].origin)
        {
            return Err(AgentError::Store(
                "root attachment change pending projection metadata changed".into(),
            ));
        }
    }
    Ok(())
}

#[async_trait]
impl Store for MemStore {
    async fn create_project(&self, project: &Project) -> Result<()> {
        validate_project_root_projection(project)
            .map_err(|message| AgentError::Store(message.into()))?;
        let mut projects = self.projects.lock().unwrap();
        if projects.contains_key(&project.id) {
            return Err(AgentError::Store("project already exists".into()));
        }
        projects.insert(project.id, project.clone());
        Ok(())
    }
    async fn get_project(&self, id: ProjectId) -> Result<Option<Project>> {
        Ok(self.projects.lock().unwrap().get(&id).cloned())
    }
    async fn list_projects(&self) -> Result<Vec<Project>> {
        Ok(self.projects.lock().unwrap().values().cloned().collect())
    }
    async fn create_document(&self, document: &DocumentRecord) -> Result<()> {
        if (document.chat_id.is_some() && document.project_id.is_some())
            || document
                .chat_id
                .is_some_and(|id| !self.chats.lock().unwrap().contains_key(&id))
            || document
                .project_id
                .is_some_and(|id| !self.projects.lock().unwrap().contains_key(&id))
        {
            return Err(AgentError::Store(
                "document references an invalid owner scope".into(),
            ));
        }
        let mut state = self.document_state.lock().unwrap();
        if state.documents.contains_key(&document.id) {
            return Err(AgentError::Store("document already exists".into()));
        }
        state.documents.insert(document.id, document.clone());
        Ok(())
    }
    async fn get_document(&self, id: DocumentId) -> Result<Option<DocumentRecord>> {
        Ok(self
            .document_state
            .lock()
            .unwrap()
            .documents
            .get(&id)
            .cloned())
    }
    async fn list_documents(&self, scope: DocumentScope) -> Result<Vec<DocumentRecord>> {
        Ok(self
            .document_state
            .lock()
            .unwrap()
            .documents
            .values()
            .filter(|document| match scope {
                DocumentScope::All => true,
                DocumentScope::Unscoped => {
                    document.chat_id.is_none() && document.project_id.is_none()
                }
                DocumentScope::Project(id) => {
                    document.chat_id.is_none() && document.project_id == Some(id)
                }
                DocumentScope::Chat(id) => document.chat_id == Some(id),
            })
            .cloned()
            .collect())
    }
    async fn list_document_summaries(
        &self,
        scope: DocumentScope,
        after: Option<DocumentListCursor>,
        limit: u64,
    ) -> Result<Vec<DocumentSummaryRecord>> {
        let mut documents: Vec<_> = self
            .document_state
            .lock()
            .unwrap()
            .documents
            .values()
            .filter(|document| match scope {
                DocumentScope::All => true,
                DocumentScope::Unscoped => {
                    document.chat_id.is_none() && document.project_id.is_none()
                }
                DocumentScope::Project(id) => {
                    document.chat_id.is_none() && document.project_id == Some(id)
                }
                DocumentScope::Chat(id) => document.chat_id == Some(id),
            })
            .filter(|document| {
                after.is_none_or(|cursor| {
                    document.created_at < cursor.created_at
                        || (document.created_at == cursor.created_at && document.id.0 < cursor.id.0)
                })
            })
            .map(document_summary)
            .collect();
        documents.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.id.0.cmp(&left.id.0))
        });
        documents.truncate(limit.try_into().unwrap_or(usize::MAX));
        Ok(documents)
    }
    async fn delete_document(&self, id: DocumentId) -> Result<()> {
        self.document_state.lock().unwrap().documents.remove(&id);
        Ok(())
    }

    async fn upsert_document(&self, document: &DocumentUpsert) -> Result<DocumentRecord> {
        if document.media_type.is_empty()
            || document.origin_uri.as_deref() == Some("")
            || (document.chat_id.is_some() && document.project_id.is_some())
            || document
                .chat_id
                .is_some_and(|id| !self.chats.lock().unwrap().contains_key(&id))
            || document
                .project_id
                .is_some_and(|id| !self.projects.lock().unwrap().contains_key(&id))
        {
            return Err(AgentError::Store("invalid document upsert".into()));
        }
        let mut state = self.document_state.lock().unwrap();
        if state.documents.get(&document.id).is_some_and(|existing| {
            existing.chat_id != document.chat_id || existing.project_id != document.project_id
        }) {
            return Err(AgentError::Store(format!(
                "document {} cannot move between project corpora",
                document.id
            )));
        }
        let created_at = state
            .documents
            .get(&document.id)
            .map_or(document.updated_at, |existing| existing.created_at);
        let source_blob = state
            .documents
            .get(&document.id)
            .and_then(|existing| existing.source_blob.clone());
        let record = DocumentRecord {
            chat_id: document.chat_id,
            id: document.id,
            project_id: document.project_id,
            origin_uri: document.origin_uri.clone(),
            media_type: document.media_type.clone(),
            title: document.title.clone(),
            source_blob,
            canonical_text: document.canonical_text.clone(),
            created_at,
            updated_at: document.updated_at,
        };
        state.documents.insert(record.id, record.clone());
        Ok(record)
    }

    async fn accept_document_source(
        &self,
        source: &DocumentSourceUpsert,
    ) -> Result<DocumentRecord> {
        if source.media_type.is_empty()
            || source.origin_uri.as_deref() == Some("")
            || !source.source_blob.has_content_addressed_id()
            || (source.chat_id.is_some() && source.project_id.is_some())
            || source
                .chat_id
                .is_some_and(|id| !self.chats.lock().unwrap().contains_key(&id))
            || source
                .project_id
                .is_some_and(|id| !self.projects.lock().unwrap().contains_key(&id))
        {
            return Err(AgentError::Store("invalid document source upsert".into()));
        }
        let mut state = self.document_state.lock().unwrap();
        if state.documents.get(&source.id).is_some_and(|existing| {
            existing.chat_id != source.chat_id || existing.project_id != source.project_id
        }) {
            return Err(AgentError::Store(format!(
                "document {} cannot move between project corpora",
                source.id
            )));
        }
        let created_at = state
            .documents
            .get(&source.id)
            .map_or(source.updated_at, |existing| existing.created_at);
        let record = DocumentRecord {
            chat_id: source.chat_id,
            id: source.id,
            project_id: source.project_id,
            origin_uri: source.origin_uri.clone(),
            media_type: source.media_type.clone(),
            title: source.title.clone(),
            source_blob: Some(source.source_blob.clone()),
            canonical_text: source.canonical_text.clone(),
            created_at,
            updated_at: source.updated_at,
        };
        state.documents.insert(record.id, record.clone());
        Ok(record)
    }

    async fn create_chat(&self, chat: &Chat) -> Result<()> {
        validate_chat_root_projection(chat).map_err(|message| AgentError::Store(message.into()))?;
        let projects = self.projects.lock().unwrap();
        let project_roots = match chat.project_id {
            Some(project_id) => projects
                .get(&project_id)
                .ok_or_else(|| AgentError::Store("chat project does not exist".into()))?
                .root_attachments
                .as_slice(),
            None => &[],
        };
        validate_chat_root_projection_against_project(chat, project_roots)
            .map_err(|message| AgentError::Store(message.into()))?;
        let mut chats = self.chats.lock().unwrap();
        if chats.contains_key(&chat.id) {
            return Err(AgentError::Store("chat already exists".into()));
        }
        chats.insert(chat.id, chat.clone());
        Ok(())
    }
    async fn create_chat_with_project_defaults(&self, base: &Chat) -> Result<Chat> {
        if base.attachment_revision != 0 || !base.root_attachments.is_empty() {
            return Err(AgentError::Store(
                "chat project defaults must start from an empty revision-zero projection".into(),
            ));
        }
        let projects = self.projects.lock().unwrap();
        let mut chat = base.clone();
        if let Some(project_id) = chat.project_id {
            let project = projects
                .get(&project_id)
                .ok_or_else(|| AgentError::Store("chat project does not exist".into()))?;
            chat.root_attachments = project
                .root_attachments
                .iter()
                .copied()
                .map(|root_id| ChatRootAttachment {
                    root_id,
                    origin: RootAttachmentOrigin::ProjectDefault,
                })
                .collect();
            if !chat.root_attachments.is_empty() {
                chat.attachment_revision = 1;
            }
        }
        validate_chat_root_projection(&chat)
            .map_err(|message| AgentError::Store(message.into()))?;
        let mut chats = self.chats.lock().unwrap();
        if chats.contains_key(&chat.id) {
            return Err(AgentError::Store("chat already exists".into()));
        }
        chats.insert(chat.id, chat.clone());
        Ok(chat)
    }
    async fn get_chat(&self, id: ChatId) -> Result<Option<Chat>> {
        Ok(self.chats.lock().unwrap().get(&id).cloned())
    }
    async fn list_chats(&self) -> Result<Vec<Chat>> {
        Ok(self.chats.lock().unwrap().values().cloned().collect())
    }
    async fn get_chat_transcript(&self, id: ChatId) -> Result<Option<ChatTranscriptSnapshot>> {
        if !self.chats.lock().unwrap().contains_key(&id) {
            return Ok(None);
        }
        let last_event_seq = self
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|(chat_id, _)| *chat_id == id)
            .map(|(_, event)| event.seq)
            .max()
            .unwrap_or(0);
        Ok(Some(ChatTranscriptSnapshot {
            messages: Vec::new(),
            message_attachments: Vec::new(),
            message_document_attachments: Vec::new(),
            citations: Vec::new(),
            message_invoked_skills: Vec::new(),
            terminal_turns: Vec::new(),
            tool_activity: Vec::new(),
            last_event_seq,
        }))
    }
    async fn set_chat_model(&self, id: ChatId, model: Option<String>) -> Result<()> {
        if let Some(chat) = self.chats.lock().unwrap().get_mut(&id) {
            chat.model = model;
        }
        Ok(())
    }
    async fn set_chat_title(&self, id: ChatId, title: Option<String>) -> Result<()> {
        if let Some(chat) = self.chats.lock().unwrap().get_mut(&id) {
            chat.title = title;
        }
        Ok(())
    }
    async fn set_chat_title_if_unset(&self, id: ChatId, title: &str) -> Result<bool> {
        let mut chats = self.chats.lock().unwrap();
        let Some(chat) = chats.get_mut(&id) else {
            return Ok(false);
        };
        if chat.title.is_some() {
            return Ok(false);
        }
        chat.title = Some(title.to_owned());
        Ok(true)
    }
    async fn update_chat_metadata(
        &self,
        id: ChatId,
        title: Option<Option<String>>,
        model: Option<Option<String>>,
        reasoning_effort: Option<Option<ReasoningEffort>>,
        permission_mode: Option<Option<PermissionMode>>,
        network_policy: Option<crate::NetworkPolicy>,
    ) -> Result<bool> {
        let mut chats = self.chats.lock().unwrap();
        let Some(chat) = chats.get_mut(&id) else {
            return Ok(false);
        };
        if let Some(title) = title {
            chat.title = title;
        }
        if let Some(model) = model {
            chat.model = model;
        }
        if let Some(reasoning_effort) = reasoning_effort {
            chat.reasoning_effort = reasoning_effort;
        }
        if let Some(permission_mode) = permission_mode {
            chat.permission_mode = permission_mode;
        }
        if let Some(network_policy) = network_policy {
            chat.network_policy = network_policy;
        }
        Ok(true)
    }
    async fn set_chat_memory_incognito(&self, id: ChatId, memory_incognito: bool) -> Result<bool> {
        let mut chats = self.chats.lock().unwrap();
        let Some(chat) = chats.get_mut(&id) else {
            return Ok(false);
        };
        chat.memory_incognito = memory_incognito;
        Ok(true)
    }
    async fn begin_root_attachment_change(
        &self,
        request: &BeginRootAttachmentChange,
    ) -> Result<BeginRootAttachmentChangeOutcome> {
        request
            .validate()
            .map_err(|message| AgentError::Store(message.into()))?;
        let created_at = canonical_root_attachment_timestamp(request.created_at)?;

        let mut chats = self.chats.lock().unwrap();
        let mut changes = self.root_attachment_changes.lock().unwrap();
        if let Some(existing) = changes.get(&request.id) {
            let exact = existing.chat_id == request.chat_id
                && existing.executor_id == request.executor_id
                && existing.root_id == request.root_id
                && existing.action == request.action
                && existing.expected_revision == request.expected_attachment_revision
                && existing.created_at == created_at;
            return Ok(if exact {
                BeginRootAttachmentChangeOutcome::Existing(existing.clone())
            } else {
                BeginRootAttachmentChangeOutcome::IdentityConflict
            });
        }

        let Some(chat) = chats.get_mut(&request.chat_id) else {
            return Ok(BeginRootAttachmentChangeOutcome::ChatNotFound);
        };
        if changes.values().any(|change| {
            change.chat_id == request.chat_id
                && change.phase == RootAttachmentChangePhase::AwaitingBroker
        }) {
            return Ok(BeginRootAttachmentChangeOutcome::ChatBusy);
        }
        if chat.attachment_revision != request.expected_attachment_revision {
            return Ok(BeginRootAttachmentChangeOutcome::RevisionConflict {
                current_attachment_revision: chat.attachment_revision,
            });
        }

        let before_revision = chat.attachment_revision;
        let existing_position = chat
            .root_attachments
            .iter()
            .position(|attachment| attachment.root_id == request.root_id);
        let projection_existed_before = existing_position.is_some();
        let (origin, projection_position) = if let Some(position) = existing_position {
            (
                Some(chat.root_attachments[position].origin),
                Some(u32::try_from(position).expect("bounded attachment position")),
            )
        } else if request.action == RootAttachmentChangeAction::Attach {
            if chat.root_attachments.len() == MAX_ROOT_ATTACHMENTS {
                return Ok(BeginRootAttachmentChangeOutcome::CapacityExceeded);
            }
            (
                Some(RootAttachmentOrigin::Conversation),
                Some(
                    u32::try_from(chat.root_attachments.len())
                        .expect("bounded attachment position"),
                ),
            )
        } else {
            (None, None)
        };

        let revisions_required = match (request.action, projection_existed_before) {
            // Reserve both the intent revision and a possible failure rollback.
            (RootAttachmentChangeAction::Attach, false) => 2,
            // Successful detach removes the projection only after broker success.
            (RootAttachmentChangeAction::Detach, true) => 1,
            _ => 0,
        };
        if before_revision > MAX_ATTACHMENT_REVISION - revisions_required {
            return Ok(BeginRootAttachmentChangeOutcome::RevisionExhausted);
        }

        let (subject_kind, subject_id) = match chat.project_id {
            Some(project_id) if project_id.as_uuid().is_nil() => {
                return Err(AgentError::Store(format!(
                    "chat {} has a nil root attachment project subject",
                    request.chat_id
                )));
            }
            Some(project_id) => (RootAttachmentSubjectKind::Project, *project_id.as_uuid()),
            None => (
                RootAttachmentSubjectKind::Conversation,
                *request.chat_id.as_uuid(),
            ),
        };
        let intent_revision =
            if request.action == RootAttachmentChangeAction::Attach && !projection_existed_before {
                chat.root_attachments.push(ChatRootAttachment {
                    root_id: request.root_id,
                    origin: RootAttachmentOrigin::Conversation,
                });
                chat.attachment_revision += 1;
                chat.attachment_revision
            } else {
                before_revision
            };
        let change = RootAttachmentChange {
            id: request.id,
            chat_id: request.chat_id,
            executor_id: request.executor_id,
            root_id: request.root_id,
            action: request.action,
            subject_kind,
            subject_id,
            origin,
            projection_position,
            projection_existed_before,
            expected_revision: request.expected_attachment_revision,
            before_revision,
            intent_revision,
            phase: RootAttachmentChangePhase::AwaitingBroker,
            result_revision: None,
            projection_changed: None,
            broker_changed: None,
            broker_currently_attached: None,
            failure: None,
            created_at,
            finished_at: None,
        };
        change
            .validate()
            .map_err(|message| AgentError::Store(message.into()))?;
        changes.insert(change.id, change.clone());
        Ok(BeginRootAttachmentChangeOutcome::Begun(change))
    }

    async fn finish_root_attachment_change(
        &self,
        id: RootAttachmentChangeId,
        executor_id: uuid::Uuid,
        terminal: &RootAttachmentChangeTerminal,
        finished_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<FinishRootAttachmentChangeOutcome> {
        if executor_id.is_nil() {
            return Err(AgentError::Store(
                "root attachment change executor id must not be nil".into(),
            ));
        }
        terminal
            .validate()
            .map_err(|message| AgentError::Store(message.into()))?;
        let finished_at = canonical_root_attachment_timestamp(finished_at)?;

        let mut chats = self.chats.lock().unwrap();
        let mut changes = self.root_attachment_changes.lock().unwrap();
        let Some(existing) = changes.get(&id).cloned() else {
            return Ok(FinishRootAttachmentChangeOutcome::NotFound);
        };
        if existing.executor_id != executor_id {
            return Ok(FinishRootAttachmentChangeOutcome::ExecutorMismatch);
        }
        if existing.phase != RootAttachmentChangePhase::AwaitingBroker {
            let exact = root_attachment_terminal_matches(&existing, terminal);
            return Ok(if exact {
                FinishRootAttachmentChangeOutcome::Existing(existing)
            } else {
                FinishRootAttachmentChangeOutcome::AlreadyTerminal(existing)
            });
        }
        // Match the database contract: caller creation time is immutable
        // identity, while server-owned finish time is clamped under the lock so
        // clock skew cannot leave the chat permanently busy.
        let finished_at = finished_at.max(existing.created_at);
        let desired_attached = existing.action == RootAttachmentChangeAction::Attach;
        let broker_state_contradicts_terminal = match terminal {
            RootAttachmentChangeTerminal::Completed {
                broker_currently_attached,
                ..
            } => *broker_currently_attached != desired_attached,
            RootAttachmentChangeTerminal::Failed {
                broker_currently_attached: Some(broker_currently_attached),
                ..
            } => *broker_currently_attached == desired_attached,
            RootAttachmentChangeTerminal::Failed {
                broker_currently_attached: None,
                ..
            } => false,
        };
        if broker_state_contradicts_terminal {
            return Ok(FinishRootAttachmentChangeOutcome::BrokerStateMismatch);
        }

        let chat = chats.get_mut(&existing.chat_id).ok_or_else(|| {
            AgentError::Store("root attachment change references a missing chat".into())
        })?;
        if chat.attachment_revision != existing.intent_revision {
            return Err(AgentError::Store(
                "root attachment change intent revision no longer matches its chat".into(),
            ));
        }
        validate_mem_pending_attachment(chat, &existing)?;

        let mut finished = existing.clone();
        match terminal {
            RootAttachmentChangeTerminal::Completed {
                broker_changed,
                broker_currently_attached,
            } => {
                let projection_changed = match existing.action {
                    RootAttachmentChangeAction::Attach => !existing.projection_existed_before,
                    RootAttachmentChangeAction::Detach if existing.projection_existed_before => {
                        remove_exact_attachment(chat, &existing)?;
                        chat.attachment_revision += 1;
                        true
                    }
                    RootAttachmentChangeAction::Detach => false,
                };
                finished.phase = RootAttachmentChangePhase::Completed;
                finished.result_revision = Some(chat.attachment_revision);
                finished.projection_changed = Some(projection_changed);
                finished.broker_changed = Some(*broker_changed);
                finished.broker_currently_attached = Some(*broker_currently_attached);
            }
            RootAttachmentChangeTerminal::Failed {
                broker_changed,
                broker_currently_attached,
                failure,
            } => {
                if existing.action == RootAttachmentChangeAction::Attach
                    && !existing.projection_existed_before
                {
                    remove_exact_attachment(chat, &existing)?;
                    chat.attachment_revision += 1;
                }
                finished.phase = RootAttachmentChangePhase::Failed;
                finished.result_revision = Some(chat.attachment_revision);
                finished.projection_changed = Some(false);
                finished.broker_changed = *broker_changed;
                finished.broker_currently_attached = *broker_currently_attached;
                finished.failure = Some(failure.clone());
            }
        }
        finished.finished_at = Some(finished_at);
        finished
            .validate()
            .map_err(|message| AgentError::Store(message.into()))?;
        changes.insert(id, finished.clone());
        Ok(FinishRootAttachmentChangeOutcome::Finished(finished))
    }

    async fn get_root_attachment_change(
        &self,
        id: RootAttachmentChangeId,
    ) -> Result<Option<RootAttachmentChange>> {
        Ok(self
            .root_attachment_changes
            .lock()
            .unwrap()
            .get(&id)
            .cloned())
    }

    async fn list_pending_root_attachment_changes(
        &self,
        executor_id: uuid::Uuid,
        limit: u64,
    ) -> Result<Vec<RootAttachmentChange>> {
        if executor_id.is_nil() || !(1..=MAX_PENDING_ROOT_ATTACHMENT_CHANGES).contains(&limit) {
            return Err(AgentError::Store(
                "invalid pending root attachment change scan".into(),
            ));
        }
        let mut pending: Vec<_> = self
            .root_attachment_changes
            .lock()
            .unwrap()
            .values()
            .filter(|change| {
                change.executor_id == executor_id
                    && change.phase == RootAttachmentChangePhase::AwaitingBroker
            })
            .cloned()
            .collect();
        pending.sort_by_key(|change| (change.created_at, *change.id.as_uuid()));
        pending.truncate(limit.try_into().expect("validated pending scan limit"));
        Ok(pending)
    }
    async fn append_message(&self, _message: &Message) -> Result<()> {
        Ok(())
    }
    async fn list_messages(&self, _chat_id: ChatId) -> Result<Vec<Message>> {
        Ok(vec![])
    }
    async fn accept_tool_call(&self, call: &ToolCallRecord) -> Result<AcceptToolCallOutcome> {
        if call.execution == ToolCallExecution::Orchestration {
            return Err(AgentError::Store(
                "orchestration tool calls require an atomic turn checkpoint".into(),
            ));
        }
        let mut calls = self.tool_calls.lock().unwrap();
        if let Some(existing) = calls.get(&call.id) {
            let matches = existing.chat_id == call.chat_id
                && existing.turn_id == call.turn_id
                && existing.provider_id == call.provider_id
                && existing.name == call.name
                && existing.arguments == call.arguments
                && existing.execution == call.execution
                && existing.created_at == call.created_at;
            return Ok(if matches {
                AcceptToolCallOutcome::Existing(existing.clone())
            } else {
                AcceptToolCallOutcome::IdentityConflict
            });
        }
        let mut history = self.tool_history_order.lock().unwrap();
        let next = history
            .values()
            .filter(|(chat_id, _)| *chat_id == call.chat_id)
            .map(|(_, order)| *order)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| AgentError::Store("tool history exhausted".into()))?;
        history.insert(call.id, (call.chat_id, next));
        calls.insert(call.id, call.clone());
        Ok(AcceptToolCallOutcome::Accepted(call.clone()))
    }
    async fn claim_client_tool_call(
        &self,
        id: CallId,
        chat_id: ChatId,
        executor_id: uuid::Uuid,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ClaimClientToolCallOutcome> {
        let mut calls = self.tool_calls.lock().unwrap();
        let Some(call) = calls.get_mut(&id) else {
            return Ok(ClaimClientToolCallOutcome::Unavailable);
        };
        if call.chat_id != chat_id
            || call.execution != ToolCallExecution::Client
            || call.status != ToolCallStatus::Pending
        {
            return Ok(ClaimClientToolCallOutcome::Unavailable);
        }
        if call.client_executor_id == Some(executor_id)
            && call
                .client_lease_expires_at
                .is_some_and(|expiry| expiry > now)
        {
            let stored_lease_token = self
                .tool_call_lease_tokens
                .lock()
                .unwrap()
                .get(&id)
                .copied()
                .ok_or_else(|| AgentError::Store("client claim token is missing".into()))?;
            if stored_lease_token != lease_token {
                return Ok(ClaimClientToolCallOutcome::Unavailable);
            }
            return Ok(ClaimClientToolCallOutcome::Existing(ClientToolCallClaim {
                call: call.clone(),
                lease_token: stored_lease_token,
            }));
        }
        if call.client_executor_id.is_some()
            || executor_id.is_nil()
            || lease_token.is_nil()
            || lease_expires_at <= now
        {
            return Ok(ClaimClientToolCallOutcome::Unavailable);
        }
        call.client_executor_id = Some(executor_id);
        self.tool_call_lease_tokens
            .lock()
            .unwrap()
            .insert(id, lease_token);
        call.client_lease_expires_at = Some(lease_expires_at);
        Ok(ClaimClientToolCallOutcome::Claimed(ClientToolCallClaim {
            call: call.clone(),
            lease_token,
        }))
    }
    async fn heartbeat_client_tool_call(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<HeartbeatClientToolCallOutcome> {
        let mut calls = self.tool_calls.lock().unwrap();
        let Some(call) = calls.get_mut(&id) else {
            return Ok(HeartbeatClientToolCallOutcome::LeaseLost);
        };
        let Some(current_expiry) = call.client_lease_expires_at else {
            return Ok(HeartbeatClientToolCallOutcome::LeaseLost);
        };
        if call.chat_id != chat_id
            || call.execution != ToolCallExecution::Client
            || call.status != ToolCallStatus::Pending
            || self
                .tool_call_lease_tokens
                .lock()
                .unwrap()
                .get(&id)
                .copied()
                != Some(lease_token)
            || current_expiry <= now
            || lease_expires_at < current_expiry
        {
            return Ok(HeartbeatClientToolCallOutcome::LeaseLost);
        }
        if lease_expires_at == current_expiry {
            return Ok(HeartbeatClientToolCallOutcome::Existing);
        }
        call.client_lease_expires_at = Some(lease_expires_at);
        Ok(HeartbeatClientToolCallOutcome::Extended)
    }
    async fn resolve_server_tool_call(
        &self,
        id: CallId,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ResolveToolCallOutcome> {
        self.resolve_mem_tool_call(id, None, resolution, resolved_at)
    }
    async fn resolve_client_tool_call(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ResolveToolCallOutcome> {
        self.resolve_mem_tool_call(
            id,
            Some((chat_id, lease_token, now, false)),
            resolution,
            resolved_at,
        )
    }
    async fn resolve_client_tool_call_and_append_event(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<JournaledClientToolCallOutcome> {
        Ok(JournaledClientToolCallOutcome {
            outcome: self.resolve_mem_tool_call(
                id,
                Some((chat_id, lease_token, now, false)),
                resolution,
                resolved_at,
            )?,
            turn: None,
            terminal_event: None,
        })
    }
    async fn resolve_expired_client_tool_call(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ResolveToolCallOutcome> {
        self.resolve_mem_tool_call(
            id,
            Some((chat_id, lease_token, now, true)),
            resolution,
            resolved_at,
        )
    }
    async fn resolve_expired_client_tool_call_and_append_event(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<JournaledClientToolCallOutcome> {
        Ok(JournaledClientToolCallOutcome {
            outcome: self.resolve_mem_tool_call(
                id,
                Some((chat_id, lease_token, now, true)),
                resolution,
                resolved_at,
            )?,
            turn: None,
            terminal_event: None,
        })
    }
    async fn list_pending_client_tool_calls(&self, chat_id: ChatId) -> Result<Vec<ToolCallRecord>> {
        let mut calls: Vec<_> = self
            .tool_calls
            .lock()
            .unwrap()
            .values()
            .filter(|call| {
                call.chat_id == chat_id
                    && call.execution == ToolCallExecution::Client
                    && call.status == ToolCallStatus::Pending
            })
            .cloned()
            .collect();
        let history = self.tool_history_order.lock().unwrap();
        calls.sort_by_key(|call| history.get(&call.id).map(|(_, order)| *order));
        Ok(calls)
    }
    async fn list_tool_calls(&self, chat_id: ChatId) -> Result<Vec<ToolCallRecord>> {
        let mut calls: Vec<_> = self
            .tool_calls
            .lock()
            .unwrap()
            .values()
            .filter(|call| call.chat_id == chat_id)
            .cloned()
            .collect();
        let history = self.tool_history_order.lock().unwrap();
        calls.sort_by_key(|call| history.get(&call.id).map(|(_, order)| *order));
        Ok(calls)
    }
    async fn get_setting(&self, key: &str) -> Result<Option<Value>> {
        Ok(self.settings.lock().unwrap().get(key).cloned())
    }
    async fn set_setting(&self, key: &str, value: &Value) -> Result<()> {
        self.settings
            .lock()
            .unwrap()
            .insert(key.to_string(), value.clone());
        Ok(())
    }
    async fn delete_setting(&self, key: &str) -> Result<()> {
        self.settings.lock().unwrap().remove(key);
        Ok(())
    }
    async fn append_event(&self, chat_id: ChatId, event: &AgentEvent) -> Result<i64> {
        let mut events = self.events.lock().unwrap();
        let seq = events.iter().filter(|(id, _)| *id == chat_id).count() as i64 + 1;
        events.push((
            chat_id,
            SequencedEvent {
                seq,
                event: event.clone(),
            },
        ));
        Ok(seq)
    }
    async fn list_events(&self, chat_id: ChatId, after: i64) -> Result<Vec<SequencedEvent>> {
        Ok(self
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|(id, e)| *id == chat_id && e.seq > after)
            .map(|(_, e)| e.clone())
            .collect())
    }

    async fn resumed_sandbox_spawn_batch(
        &self,
        _turn_id: crate::TurnId,
        _attempt_count: i32,
        _claim_count: i32,
    ) -> Result<Vec<crate::agent::SandboxAgentSpawnRequest>> {
        Ok(Vec::new())
    }
}

fn document_summary(document: &DocumentRecord) -> DocumentSummaryRecord {
    DocumentSummaryRecord {
        chat_id: document.chat_id,
        id: document.id,
        project_id: document.project_id,
        origin_uri: document.origin_uri.clone(),
        media_type: document.media_type.clone(),
        title: document.title.clone(),
        source_byte_len: document.source_blob.as_ref().map(|blob| blob.byte_len),
        readable: document.is_readable(),
        created_at: document.created_at,
        updated_at: document.updated_at,
    }
}

#[test]
fn mem_store_create_document_rejects_an_unknown_project() {
    let store = MemStore::default();
    let now = chrono::Utc::now();
    let document = DocumentRecord {
        chat_id: None,
        id: DocumentId::new(),
        project_id: Some(ProjectId::new()),
        origin_uri: None,
        media_type: "text/plain".into(),
        title: None,
        source_blob: None,
        canonical_text: "orphan".into(),
        created_at: now,
        updated_at: now,
    };

    assert!(block_on(store.create_document(&document)).is_err());
    assert_eq!(block_on(store.get_document(document.id)).unwrap(), None);
}

#[test]
fn store_is_object_safe_and_roundtrips() {
    let store: Arc<dyn Store> = Arc::new(MemStore::default());
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap(),
    };
    block_on(store.create_chat(&chat)).unwrap();
    let fetched = block_on(store.get_chat(chat.id)).unwrap();
    assert_eq!(fetched.as_ref(), Some(&chat));

    block_on(store.set_setting("model", &serde_json::json!("claude"))).unwrap();
    assert_eq!(
        block_on(store.get_setting("model")).unwrap(),
        Some(serde_json::json!("claude"))
    );

    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        origin_uri: Some("file:///mem-store.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "atomic source".into(),
        updated_at: chrono::DateTime::<chrono::Utc>::from_timestamp(1, 0).unwrap(),
    };
    let published = block_on(store.upsert_document(&source)).unwrap();
    assert_eq!(
        block_on(store.get_document(source.id)).unwrap(),
        Some(published)
    );

    block_on(store.delete_document(source.id)).unwrap();
    block_on(store.delete_document(source.id)).unwrap();
    assert_eq!(block_on(store.get_document(source.id)).unwrap(), None);
}

#[test]
fn custom_store_atomic_chat_default_fails_closed() {
    let store: Arc<dyn Store> = Arc::new(MemStore::default());
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap(),
    };
    let owner = crate::model::OwnerId::local();

    let created =
        block_on(store.create_chat_with_project_defaults_and_settings_scoped(&owner, &chat, &[]))
            .unwrap();
    assert_eq!(created, chat);

    let rejected = Chat {
        id: ChatId::new(),
        ..chat
    };
    block_on(store.set_setting("model", &serde_json::json!("before"))).unwrap();
    let error = block_on(store.create_chat_with_project_defaults_and_settings_scoped(
        &owner,
        &rejected,
        &[("model".into(), serde_json::json!("after"))],
    ))
    .unwrap_err();
    assert!(matches!(
        error,
        AgentError::Store(message)
            if message == "atomic chat creation with setting updates is not implemented by this Store"
    ));
    assert_eq!(block_on(store.get_chat(rejected.id)).unwrap(), None);
    assert_eq!(
        block_on(store.get_setting("model")).unwrap(),
        Some(serde_json::json!("before"))
    );
}

#[test]
fn mem_store_rejects_moving_a_live_document_between_corpora() {
    let store: Arc<dyn Store> = Arc::new(MemStore::default());
    let project_a = Project {
        id: ProjectId::new(),
        title: None,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: chrono::Utc::now(),
    };
    let project_b = Project {
        id: ProjectId::new(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        ..project_a.clone()
    };
    block_on(store.create_project(&project_a)).unwrap();
    block_on(store.create_project(&project_b)).unwrap();
    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: Some(project_a.id),
        origin_uri: Some("file:///scoped.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "project A source".into(),
        updated_at: chrono::Utc::now(),
    };
    let first = block_on(store.upsert_document(&source)).unwrap();
    let moved = DocumentUpsert {
        project_id: Some(project_b.id),
        canonical_text: "must not move".into(),
        ..source
    };
    assert!(block_on(store.upsert_document(&moved)).is_err());
    assert_eq!(block_on(store.get_document(moved.id)).unwrap(), Some(first));
}
