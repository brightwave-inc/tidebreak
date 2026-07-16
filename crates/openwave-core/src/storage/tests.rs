use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::Mutex;

use futures::executor::block_on;

use super::*;
mod root_attachment;

use crate::model::{
    validate_chat_root_projection, validate_chat_root_projection_against_project,
    validate_project_root_projection, ChatRootAttachment, DocumentJobKind, DocumentJobStatus,
    DocumentProcessingStatus, RootAttachmentChangeAction, RootAttachmentChangeFailure,
    RootAttachmentChangePhase, RootAttachmentOrigin, RootAttachmentSubjectKind, ToolCallExecution,
    ToolCallStatus, MAX_ATTACHMENT_REVISION, MAX_ROOT_ATTACHMENTS,
};

/// Minimal in-memory `Store` — proves the trait is object-safe and usable
/// behind `Arc<dyn Store>`, and exercises the signatures.
#[derive(Default)]
struct MemDocumentState {
    documents: HashMap<DocumentId, DocumentRecord>,
    generations: HashMap<DocumentId, DocumentGeneration>,
    tombstones: HashSet<DocumentId>,
    pending_retirements: HashMap<DocumentId, DocumentGeneration>,
    jobs: HashMap<DocumentJobId, DocumentJob>,
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

fn allocate_mem_generation(
    state: &mut MemDocumentState,
    id: DocumentId,
) -> Result<DocumentGeneration> {
    let content_revision = match state.generations.get(&id) {
        Some(current) => current
            .content_revision
            .checked_add(1)
            .ok_or_else(|| AgentError::Store(format!("document {id} revision overflow")))?,
        None => 1,
    };
    let generation = DocumentGeneration {
        content_revision,
        revision_token: uuid::Uuid::new_v4(),
    };
    state.generations.insert(id, generation);
    state.tombstones.remove(&id);
    Ok(generation)
}

fn reset_mem_document_job(
    job: &mut DocumentJob,
    max_attempts: i32,
    now: chrono::DateTime<chrono::Utc>,
) {
    job.status = DocumentJobStatus::Queued;
    job.attempt_count = 0;
    job.max_attempts = max_attempts;
    job.available_at = now;
    job.lease_token = None;
    job.lease_expires_at = None;
    job.started_at = None;
    job.finished_at = None;
    job.last_error_code = None;
    job.last_error_detail = None;
    job.updated_at = now;
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
        if document
            .project_id
            .is_some_and(|id| !self.projects.lock().unwrap().contains_key(&id))
        {
            return Err(AgentError::Store(
                "document references an unknown project".into(),
            ));
        }
        let mut state = self.document_state.lock().unwrap();
        if state.generations.contains_key(&document.id) {
            return Err(AgentError::Store("document already exists".into()));
        }
        let mut document = document.clone();
        document.revision_token = uuid::Uuid::new_v4();
        state.generations.insert(document.id, document.generation());
        state.tombstones.remove(&document.id);
        state.documents.insert(document.id, document);
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
                DocumentScope::Unscoped => document.project_id.is_none(),
                DocumentScope::Project(id) => document.project_id == Some(id),
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
                DocumentScope::Unscoped => document.project_id.is_none(),
                DocumentScope::Project(id) => document.project_id == Some(id),
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
    async fn get_document_generation(&self, id: DocumentId) -> Result<Option<DocumentGeneration>> {
        Ok(self
            .document_state
            .lock()
            .unwrap()
            .generations
            .get(&id)
            .copied())
    }
    async fn delete_document(&self, id: DocumentId) -> Result<DocumentGeneration> {
        let mut state = self.document_state.lock().unwrap();
        let generation = if state.documents.contains_key(&id) || !state.tombstones.contains(&id) {
            let generation = allocate_mem_generation(&mut state, id)?;
            state.documents.remove(&id);
            state.tombstones.insert(id);
            state.pending_retirements.insert(id, generation);
            generation
        } else if let Some(generation) = state.generations.get(&id) {
            *generation
        } else {
            unreachable!("a tombstone always retains its generation")
        };
        state.jobs.retain(|_, job| job.document_id != id);
        Ok(generation)
    }

    async fn list_pending_document_retirements(
        &self,
        after: Option<DocumentId>,
        limit: u64,
    ) -> Result<Vec<(DocumentId, DocumentGeneration)>> {
        let state = self.document_state.lock().unwrap();
        let mut retirements: Vec<_> = state
            .pending_retirements
            .iter()
            .filter(|(id, _)| after.is_none_or(|after| id.0 > after.0))
            .map(|(id, generation)| (*id, *generation))
            .collect();
        retirements.sort_unstable_by_key(|(id, _)| id.0);
        retirements.truncate(limit.try_into().unwrap_or(usize::MAX));
        Ok(retirements)
    }

    async fn get_pending_document_retirement(
        &self,
        id: DocumentId,
    ) -> Result<Option<DocumentGeneration>> {
        Ok(self
            .document_state
            .lock()
            .unwrap()
            .pending_retirements
            .get(&id)
            .copied())
    }

    async fn complete_document_retirement(
        &self,
        id: DocumentId,
        generation: DocumentGeneration,
    ) -> Result<bool> {
        let mut state = self.document_state.lock().unwrap();
        if state.pending_retirements.get(&id) != Some(&generation) {
            return Ok(false);
        }
        state.pending_retirements.remove(&id);
        Ok(true)
    }
    async fn upsert_document(&self, document: &DocumentUpsert) -> Result<DocumentRecord> {
        crate::model::validate_source_regions(&document.canonical_text, &document.source_regions)
            .map_err(|message| AgentError::Store(message.into()))?;
        if document.media_type.is_empty()
            || document.source_uri.as_deref() == Some("")
            || document
                .project_id
                .is_some_and(|id| !self.projects.lock().unwrap().contains_key(&id))
        {
            return Err(AgentError::Store("invalid document upsert".into()));
        }
        let mut state = self.document_state.lock().unwrap();
        if state
            .documents
            .get(&document.id)
            .is_some_and(|existing| existing.project_id != document.project_id)
        {
            return Err(AgentError::Store(format!(
                "document {} cannot move between project corpora",
                document.id
            )));
        }
        let created_at = state
            .documents
            .get(&document.id)
            .map_or(document.updated_at, |existing| existing.created_at);
        let generation = allocate_mem_generation(&mut state, document.id)?;
        let record = DocumentRecord {
            id: document.id,
            project_id: document.project_id,
            source_uri: document.source_uri.clone(),
            media_type: document.media_type.clone(),
            title: document.title.clone(),
            source_blob: None,
            canonical_text: document.canonical_text.clone(),
            canonical_fingerprint: None,
            source_regions: document.source_regions.clone(),
            content_revision: generation.content_revision,
            revision_token: generation.revision_token,
            processing_status: DocumentProcessingStatus::Queued,
            indexed_revision: None,
            index_fingerprint: None,
            created_at,
            updated_at: document.updated_at,
            indexed_at: None,
        };
        state.documents.insert(record.id, record.clone());
        Ok(record)
    }
    async fn upsert_document_and_enqueue_index(
        &self,
        document: &DocumentUpsert,
        pipeline_fingerprint: &str,
        max_attempts: i32,
    ) -> Result<(DocumentRecord, DocumentJob)> {
        crate::model::validate_source_regions(&document.canonical_text, &document.source_regions)
            .map_err(|message| AgentError::Store(message.into()))?;
        if pipeline_fingerprint.is_empty()
            || pipeline_fingerprint.chars().count() > DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN
            || max_attempts < 1
            || document.media_type.is_empty()
            || document.source_uri.as_deref() == Some("")
            || document
                .project_id
                .is_some_and(|id| !self.projects.lock().unwrap().contains_key(&id))
        {
            return Err(AgentError::Store("invalid document job enqueue".into()));
        }

        let mut state = self.document_state.lock().unwrap();
        if state
            .documents
            .get(&document.id)
            .is_some_and(|existing| existing.project_id != document.project_id)
        {
            return Err(AgentError::Store(format!(
                "document {} cannot move between project corpora",
                document.id
            )));
        }
        if let Some(existing) = state.documents.get(&document.id).filter(|existing| {
            existing.project_id == document.project_id
                && existing.source_uri == document.source_uri
                && existing.media_type == document.media_type
                && existing.title == document.title
                && existing.canonical_text == document.canonical_text
                && existing.source_regions == document.source_regions
        }) {
            if let Some(job) = state.jobs.values().find(|job| {
                job.document_id == existing.id
                    && job.content_revision == existing.content_revision
                    && job.revision_token == existing.revision_token
                    && job.kind == DocumentJobKind::Index
                    && job.pipeline_fingerprint == pipeline_fingerprint
            }) {
                return Ok((existing.clone(), job.clone()));
            }
        }

        let workflow_now = chrono::Utc::now();
        let created_at = state
            .documents
            .get(&document.id)
            .map_or(document.updated_at, |existing| existing.created_at);
        let generation = allocate_mem_generation(&mut state, document.id)?;
        let record = DocumentRecord {
            id: document.id,
            project_id: document.project_id,
            source_uri: document.source_uri.clone(),
            media_type: document.media_type.clone(),
            title: document.title.clone(),
            source_blob: None,
            canonical_text: document.canonical_text.clone(),
            canonical_fingerprint: None,
            source_regions: document.source_regions.clone(),
            content_revision: generation.content_revision,
            revision_token: generation.revision_token,
            processing_status: DocumentProcessingStatus::Queued,
            indexed_revision: None,
            index_fingerprint: None,
            created_at,
            updated_at: document.updated_at,
            indexed_at: None,
        };
        state.documents.insert(record.id, record.clone());

        for job in state.jobs.values_mut().filter(|job| {
            job.document_id == record.id
                && matches!(
                    job.status,
                    DocumentJobStatus::Queued
                        | DocumentJobStatus::Running
                        | DocumentJobStatus::RetryWait
                )
        }) {
            job.status = DocumentJobStatus::Cancelled;
            job.lease_token = None;
            job.lease_expires_at = None;
            job.finished_at = Some(workflow_now);
            job.updated_at = workflow_now;
        }
        let job = DocumentJob {
            id: DocumentJobId::new(),
            document_id: record.id,
            content_revision: record.content_revision,
            revision_token: record.revision_token,
            kind: DocumentJobKind::Index,
            status: DocumentJobStatus::Queued,
            pipeline_fingerprint: pipeline_fingerprint.into(),
            attempt_count: 0,
            max_attempts,
            available_at: workflow_now,
            lease_token: None,
            lease_expires_at: None,
            started_at: None,
            finished_at: None,
            last_error_code: None,
            last_error_detail: None,
            created_at: workflow_now,
            updated_at: workflow_now,
        };
        state.jobs.insert(job.id, job.clone());
        Ok((record, job))
    }
    async fn ensure_document_index_job(
        &self,
        document_id: DocumentId,
        expected_generation: DocumentGeneration,
        pipeline_fingerprint: &str,
        max_attempts: i32,
        reason: DocumentIndexJobReason,
    ) -> Result<EnsureDocumentIndexJobOutcome> {
        if pipeline_fingerprint.is_empty()
            || pipeline_fingerprint.chars().count() > DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN
            || max_attempts < 1
        {
            return Err(AgentError::Store(
                "invalid document index-job maintenance request".into(),
            ));
        }

        let mut state = self.document_state.lock().unwrap();
        let Some(mut document) = state.documents.get(&document_id).cloned() else {
            return Ok(EnsureDocumentIndexJobOutcome::MissingDocument);
        };

        if document.generation() != expected_generation {
            if reason.advances_generation()
                && expected_generation
                    .content_revision
                    .checked_add(1)
                    .is_some_and(|revision| revision == document.content_revision)
            {
                if let Some(job) = state.jobs.values().find(|job| {
                    job.document_id == document_id
                        && job.generation() == document.generation()
                        && job.kind == DocumentJobKind::Index
                        && job.pipeline_fingerprint == pipeline_fingerprint
                }) {
                    return Ok(if job.status == DocumentJobStatus::Failed {
                        EnsureDocumentIndexJobOutcome::Failed(job.clone())
                    } else {
                        EnsureDocumentIndexJobOutcome::Existing(job.clone())
                    });
                }
            }
            return Ok(EnsureDocumentIndexJobOutcome::GenerationChanged(
                document.generation(),
            ));
        }

        if document.source_blob.is_some() && document.canonical_fingerprint.is_none() {
            let parse_job = state
                .jobs
                .values()
                .filter(|job| {
                    job.document_id == document_id
                        && job.generation() == document.generation()
                        && job.kind == DocumentJobKind::Parse
                })
                .max_by_key(|job| (job.created_at, job.id.0))
                .cloned()
                .ok_or_else(|| {
                    AgentError::Store(format!(
                        "unparsed document {document_id} has no current parse job"
                    ))
                })?;
            return Ok(if parse_job.status == DocumentJobStatus::Failed {
                EnsureDocumentIndexJobOutcome::Failed(parse_job)
            } else if matches!(
                parse_job.status,
                DocumentJobStatus::Queued
                    | DocumentJobStatus::Running
                    | DocumentJobStatus::RetryWait
            ) {
                EnsureDocumentIndexJobOutcome::Parsing(parse_job)
            } else {
                return Err(AgentError::Store(format!(
                    "unparsed document {document_id} has terminal parse state {}",
                    parse_job.status.as_str()
                )));
            });
        }

        let desired_job_id = state.jobs.values().find_map(|job| {
            (job.document_id == document_id
                && job.generation() == document.generation()
                && job.kind == DocumentJobKind::Index
                && job.pipeline_fingerprint == pipeline_fingerprint)
                .then_some(job.id)
        });
        if let Some(job_id) = desired_job_id {
            let job = state.jobs.get(&job_id).unwrap().clone();
            if matches!(
                job.status,
                DocumentJobStatus::Queued
                    | DocumentJobStatus::Running
                    | DocumentJobStatus::RetryWait
            ) || (reason == DocumentIndexJobReason::PipelineChanged
                && job.status == DocumentJobStatus::Succeeded)
            {
                return Ok(EnsureDocumentIndexJobOutcome::Existing(job));
            }
            if job.status == DocumentJobStatus::Failed {
                return Ok(EnsureDocumentIndexJobOutcome::Failed(job));
            }
            if reason == DocumentIndexJobReason::DerivedStateMissing {
                let now = chrono::Utc::now();
                let job = state.jobs.get_mut(&job_id).unwrap();
                reset_mem_document_job(job, max_attempts, now);
                let job = job.clone();
                document.processing_status = DocumentProcessingStatus::Queued;
                document.indexed_revision = None;
                document.index_fingerprint = None;
                document.indexed_at = None;
                state.documents.insert(document_id, document);
                return Ok(EnsureDocumentIndexJobOutcome::Enqueued(job));
            }
        }

        if reason.advances_generation() {
            let generation = allocate_mem_generation(&mut state, document_id)?;
            document.content_revision = generation.content_revision;
            document.revision_token = generation.revision_token;
        }
        document.processing_status = DocumentProcessingStatus::Queued;
        document.indexed_revision = None;
        document.index_fingerprint = None;
        document.indexed_at = None;
        state.documents.insert(document_id, document.clone());

        let now = chrono::Utc::now();
        for job in state.jobs.values_mut().filter(|job| {
            job.document_id == document_id
                && matches!(
                    job.status,
                    DocumentJobStatus::Queued
                        | DocumentJobStatus::Running
                        | DocumentJobStatus::RetryWait
                )
        }) {
            job.status = DocumentJobStatus::Cancelled;
            job.lease_token = None;
            job.lease_expires_at = None;
            job.finished_at = Some(now);
            job.updated_at = now;
        }
        let job = DocumentJob {
            id: DocumentJobId::new(),
            document_id,
            content_revision: document.content_revision,
            revision_token: document.revision_token,
            kind: DocumentJobKind::Index,
            status: DocumentJobStatus::Queued,
            pipeline_fingerprint: pipeline_fingerprint.into(),
            attempt_count: 0,
            max_attempts,
            available_at: now,
            lease_token: None,
            lease_expires_at: None,
            started_at: None,
            finished_at: None,
            last_error_code: None,
            last_error_detail: None,
            created_at: now,
            updated_at: now,
        };
        state.jobs.insert(job.id, job.clone());
        Ok(EnsureDocumentIndexJobOutcome::Enqueued(job))
    }
    async fn ensure_document_parse_job(
        &self,
        document_id: DocumentId,
        expected_generation: DocumentGeneration,
        pipeline_fingerprint: &str,
        max_attempts: i32,
    ) -> Result<EnsureDocumentParseJobOutcome> {
        if pipeline_fingerprint.is_empty()
            || pipeline_fingerprint.chars().count() > DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN
            || max_attempts < 1
        {
            return Err(AgentError::Store(
                "invalid document parse-job maintenance request".into(),
            ));
        }

        let mut state = self.document_state.lock().unwrap();
        let Some(mut document) = state.documents.get(&document_id).cloned() else {
            return Ok(EnsureDocumentParseJobOutcome::MissingDocument);
        };
        if document.generation() != expected_generation {
            return Ok(EnsureDocumentParseJobOutcome::GenerationChanged(
                document.generation(),
            ));
        }
        if document.canonical_fingerprint.as_deref() == Some(pipeline_fingerprint) {
            return Ok(EnsureDocumentParseJobOutcome::CanonicalCurrent);
        }
        if document.source_blob.is_none() {
            return Ok(EnsureDocumentParseJobOutcome::SourceUnavailable);
        }
        if let Some(job) = state.jobs.values().find(|job| {
            job.document_id == document_id
                && job.generation() == document.generation()
                && job.kind == DocumentJobKind::Parse
                && job.pipeline_fingerprint == pipeline_fingerprint
        }) {
            return Ok(if job.status == DocumentJobStatus::Failed {
                EnsureDocumentParseJobOutcome::Failed(job.clone())
            } else if matches!(
                job.status,
                DocumentJobStatus::Queued
                    | DocumentJobStatus::Running
                    | DocumentJobStatus::RetryWait
            ) {
                EnsureDocumentParseJobOutcome::Existing(job.clone())
            } else {
                return Err(AgentError::Store(format!(
                    "document {document_id} has desired parse job {} in terminal state {} without matching canonical output",
                    job.id,
                    job.status.as_str()
                )));
            });
        }

        let has_current_parse_job = state.jobs.values().any(|job| {
            job.document_id == document_id
                && job.generation() == document.generation()
                && job.kind == DocumentJobKind::Parse
        });
        if document.canonical_fingerprint.is_some() || has_current_parse_job {
            let generation = allocate_mem_generation(&mut state, document_id)?;
            document.content_revision = generation.content_revision;
            document.revision_token = generation.revision_token;
        }
        document.canonical_text.clear();
        document.canonical_fingerprint = None;
        document.source_regions.clear();
        document.processing_status = DocumentProcessingStatus::Queued;
        document.indexed_revision = None;
        document.index_fingerprint = None;
        document.indexed_at = None;
        state.documents.insert(document_id, document.clone());

        let now = chrono::Utc::now();
        for job in state.jobs.values_mut().filter(|job| {
            job.document_id == document_id
                && matches!(
                    job.status,
                    DocumentJobStatus::Queued
                        | DocumentJobStatus::Running
                        | DocumentJobStatus::RetryWait
                )
        }) {
            job.status = DocumentJobStatus::Cancelled;
            job.lease_token = None;
            job.lease_expires_at = None;
            job.finished_at = Some(now);
            job.updated_at = now;
        }
        let job = DocumentJob {
            id: DocumentJobId::new(),
            document_id,
            content_revision: document.content_revision,
            revision_token: document.revision_token,
            kind: DocumentJobKind::Parse,
            status: DocumentJobStatus::Queued,
            pipeline_fingerprint: pipeline_fingerprint.into(),
            attempt_count: 0,
            max_attempts,
            available_at: now,
            lease_token: None,
            lease_expires_at: None,
            started_at: None,
            finished_at: None,
            last_error_code: None,
            last_error_detail: None,
            created_at: now,
            updated_at: now,
        };
        state.jobs.insert(job.id, job.clone());
        Ok(EnsureDocumentParseJobOutcome::Enqueued(job))
    }
    async fn get_document_job(&self, id: DocumentJobId) -> Result<Option<DocumentJob>> {
        Ok(self.document_state.lock().unwrap().jobs.get(&id).cloned())
    }
    async fn list_document_jobs(&self, document_id: DocumentId) -> Result<Vec<DocumentJob>> {
        let mut jobs: Vec<_> = self
            .document_state
            .lock()
            .unwrap()
            .jobs
            .values()
            .filter(|job| job.document_id == document_id)
            .cloned()
            .collect();
        jobs.sort_by(|left, right| {
            left.content_revision
                .cmp(&right.content_revision)
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.id.0.cmp(&right.id.0))
        });
        Ok(jobs)
    }
    async fn retry_document_job(
        &self,
        document_id: DocumentId,
        expected_generation: DocumentGeneration,
        kind: DocumentJobKind,
        pipeline_fingerprint: &str,
        max_attempts: i32,
    ) -> Result<Option<DocumentJob>> {
        if pipeline_fingerprint.is_empty()
            || pipeline_fingerprint.chars().count() > DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN
            || max_attempts < 1
        {
            return Err(AgentError::Store("invalid document job retry".into()));
        }
        let mut state = self.document_state.lock().unwrap();
        let Some(document) = state.documents.get(&document_id).cloned() else {
            return Ok(None);
        };
        if document.generation() != expected_generation {
            return Ok(None);
        }
        let awaiting_parse =
            document.source_blob.is_some() && document.canonical_fingerprint.is_none();
        let stage_matches = match kind {
            DocumentJobKind::Parse => awaiting_parse,
            DocumentJobKind::Index => !awaiting_parse,
        };
        if !stage_matches {
            return Ok(None);
        }
        let candidate_id = state
            .jobs
            .values()
            .find(|job| {
                job.document_id == document_id
                    && job.content_revision == document.content_revision
                    && job.revision_token == document.revision_token
                    && job.kind == kind
                    && job.pipeline_fingerprint == pipeline_fingerprint
            })
            .map(|job| job.id);
        let Some(candidate_id) = candidate_id else {
            return Ok(None);
        };
        let candidate = state.jobs.get(&candidate_id).unwrap().clone();
        if matches!(
            candidate.status,
            DocumentJobStatus::Queued | DocumentJobStatus::Running | DocumentJobStatus::RetryWait
        ) {
            let expected = if candidate.status == DocumentJobStatus::Running {
                DocumentProcessingStatus::Processing
            } else {
                DocumentProcessingStatus::Queued
            };
            if document.processing_status != expected {
                return Err(AgentError::Store(format!(
                    "document job {} is {} but exact document {} is unexpectedly {}",
                    candidate.id,
                    candidate.status.as_str(),
                    document_id,
                    document.processing_status.as_str()
                )));
            }
            return Ok(Some(candidate));
        }
        if candidate.status != DocumentJobStatus::Failed {
            return Ok(None);
        }
        if document.processing_status != DocumentProcessingStatus::Failed {
            return Err(AgentError::Store(format!(
                "failed document job {} does not match failed document {}",
                candidate.id, document_id
            )));
        }

        let now = chrono::Utc::now();
        let job = state.jobs.get_mut(&candidate_id).unwrap();
        job.status = DocumentJobStatus::Queued;
        job.attempt_count = 0;
        job.max_attempts = max_attempts;
        job.available_at = now;
        job.lease_token = None;
        job.lease_expires_at = None;
        job.started_at = None;
        job.finished_at = None;
        job.last_error_code = None;
        job.last_error_detail = None;
        job.updated_at = now;
        let job = job.clone();
        let document = state.documents.get_mut(&document_id).unwrap();
        document.processing_status = DocumentProcessingStatus::Queued;
        document.indexed_revision = None;
        document.index_fingerprint = None;
        document.indexed_at = None;
        Ok(Some(job))
    }
    async fn claim_document_job(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<DocumentJob>> {
        if lease_expires_at <= now {
            return Err(AgentError::Store(
                "document job lease expiry must be after claim time".into(),
            ));
        }
        let mut state = self.document_state.lock().unwrap();
        loop {
            let candidate_id = state
                .jobs
                .values()
                .filter(|job| {
                    (matches!(
                        job.status,
                        DocumentJobStatus::Queued | DocumentJobStatus::RetryWait
                    ) && job.available_at <= now
                        && job.attempt_count < job.max_attempts)
                        || (job.status == DocumentJobStatus::Running
                            && job.lease_expires_at.is_some_and(|expiry| expiry <= now))
                })
                .min_by(|left, right| {
                    let left_due = left.lease_expires_at.unwrap_or(left.available_at);
                    let right_due = right.lease_expires_at.unwrap_or(right.available_at);
                    left_due
                        .cmp(&right_due)
                        .then_with(|| left.created_at.cmp(&right.created_at))
                        .then_with(|| left.id.0.cmp(&right.id.0))
                })
                .map(|job| job.id);
            let Some(candidate_id) = candidate_id else {
                return Ok(None);
            };
            let candidate = state.jobs.get(&candidate_id).unwrap().clone();
            let expected_document_status = if candidate.status == DocumentJobStatus::Running {
                DocumentProcessingStatus::Processing
            } else {
                DocumentProcessingStatus::Queued
            };
            let identity_matches =
                state
                    .documents
                    .get(&candidate.document_id)
                    .is_some_and(|document| {
                        document.content_revision == candidate.content_revision
                            && document.revision_token == candidate.revision_token
                    });
            if !identity_matches {
                let job = state.jobs.get_mut(&candidate_id).unwrap();
                job.status = DocumentJobStatus::Cancelled;
                job.lease_token = None;
                job.lease_expires_at = None;
                job.finished_at = Some(now);
                job.updated_at = now;
                continue;
            }
            let current_status = state
                .documents
                .get(&candidate.document_id)
                .unwrap()
                .processing_status;
            if current_status != expected_document_status {
                return Err(AgentError::Store(format!(
                    "document job {} is {} but exact document {} is unexpectedly {}",
                    candidate.id,
                    candidate.status.as_str(),
                    candidate.document_id,
                    current_status.as_str()
                )));
            }

            if candidate.status == DocumentJobStatus::Running
                && candidate.attempt_count >= candidate.max_attempts
            {
                let job = state.jobs.get_mut(&candidate_id).unwrap();
                job.status = DocumentJobStatus::Failed;
                job.lease_token = None;
                job.lease_expires_at = None;
                job.finished_at = Some(now);
                job.last_error_code = Some("lease_expired".into());
                job.last_error_detail = Some("final worker lease expired".into());
                job.updated_at = now;
                state
                    .documents
                    .get_mut(&candidate.document_id)
                    .unwrap()
                    .processing_status = DocumentProcessingStatus::Failed;
                continue;
            }

            let job = state.jobs.get_mut(&candidate_id).unwrap();
            job.status = DocumentJobStatus::Running;
            job.attempt_count = job.attempt_count.checked_add(1).ok_or_else(|| {
                AgentError::Store(format!("document job {} attempt overflow", job.id))
            })?;
            job.lease_token = Some(uuid::Uuid::new_v4());
            job.lease_expires_at = Some(lease_expires_at);
            job.started_at.get_or_insert(now);
            if candidate.status == DocumentJobStatus::Running {
                job.last_error_code = Some("lease_expired".into());
                job.last_error_detail = Some("previous worker lease expired".into());
            }
            job.updated_at = now;
            let job = job.clone();
            state
                .documents
                .get_mut(&candidate.document_id)
                .unwrap()
                .processing_status = DocumentProcessingStatus::Processing;
            return Ok(Some(job));
        }
    }
    async fn heartbeat_document_job(
        &self,
        id: DocumentJobId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        if lease_expires_at <= now {
            return Err(AgentError::Store(
                "document job lease expiry must be after heartbeat time".into(),
            ));
        }
        let mut state = self.document_state.lock().unwrap();
        let Some(job) = state.jobs.get_mut(&id) else {
            return Ok(false);
        };
        if job.status != DocumentJobStatus::Running
            || job.lease_token != Some(lease_token)
            || job.lease_expires_at.is_none_or(|expiry| expiry <= now)
            || job.updated_at > now
            || job
                .lease_expires_at
                .is_some_and(|expiry| expiry >= lease_expires_at)
        {
            return Ok(false);
        }
        job.lease_expires_at = Some(lease_expires_at);
        job.updated_at = now;
        Ok(true)
    }
    async fn complete_document_index_job(
        &self,
        id: DocumentJobId,
        lease_token: uuid::Uuid,
        completed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        let mut state = self.document_state.lock().unwrap();
        let Some(candidate) = state.jobs.get(&id).cloned() else {
            return Ok(false);
        };
        if candidate.kind != DocumentJobKind::Index {
            return Err(AgentError::Store(format!(
                "document job {id} is not an index job"
            )));
        }
        if candidate.status != DocumentJobStatus::Running
            || candidate.lease_token != Some(lease_token)
            || candidate
                .lease_expires_at
                .is_none_or(|expiry| expiry <= completed_at)
            || candidate.updated_at > completed_at
        {
            return Ok(false);
        }
        let document_matches =
            state
                .documents
                .get(&candidate.document_id)
                .is_some_and(|document| {
                    document.content_revision == candidate.content_revision
                        && document.revision_token == candidate.revision_token
                        && document.processing_status == DocumentProcessingStatus::Processing
                });
        if !document_matches {
            return Err(AgentError::Store(format!(
                "running document job {} does not match its exact processing document {}",
                candidate.id, candidate.document_id
            )));
        }

        let job = state.jobs.get_mut(&id).unwrap();
        job.status = DocumentJobStatus::Succeeded;
        job.lease_token = None;
        job.lease_expires_at = None;
        job.finished_at = Some(completed_at);
        job.last_error_code = None;
        job.last_error_detail = None;
        job.updated_at = completed_at;
        let document = state.documents.get_mut(&candidate.document_id).unwrap();
        document.processing_status = DocumentProcessingStatus::Ready;
        document.indexed_revision = Some(candidate.content_revision);
        document.index_fingerprint = Some(candidate.pipeline_fingerprint);
        document.indexed_at = Some(completed_at);
        Ok(true)
    }
    async fn record_document_job_failure(
        &self,
        id: DocumentJobId,
        lease_token: uuid::Uuid,
        failed_at: chrono::DateTime<chrono::Utc>,
        retry_at: Option<chrono::DateTime<chrono::Utc>>,
        error_code: &str,
        error_detail: Option<&str>,
    ) -> Result<Option<DocumentJobStatus>> {
        let code_len = error_code.chars().count();
        if !(1..=DocumentJob::MAX_ERROR_CODE_LEN).contains(&code_len)
            || error_detail.is_some_and(|detail| {
                !(1..=DocumentJob::MAX_ERROR_DETAIL_LEN).contains(&detail.chars().count())
            })
            || retry_at.is_some_and(|retry_at| retry_at <= failed_at)
        {
            return Err(AgentError::Store("invalid document job failure".into()));
        }
        let mut state = self.document_state.lock().unwrap();
        let Some(candidate) = state.jobs.get(&id).cloned() else {
            return Ok(None);
        };
        if candidate.status != DocumentJobStatus::Running
            || candidate.lease_token != Some(lease_token)
            || candidate
                .lease_expires_at
                .is_none_or(|expiry| expiry <= failed_at)
            || candidate.updated_at > failed_at
        {
            return Ok(None);
        }
        let document_matches =
            state
                .documents
                .get(&candidate.document_id)
                .is_some_and(|document| {
                    document.content_revision == candidate.content_revision
                        && document.revision_token == candidate.revision_token
                        && document.processing_status == DocumentProcessingStatus::Processing
                });
        if !document_matches {
            return Err(AgentError::Store(format!(
                "running document job {} does not match its exact processing document {}",
                candidate.id, candidate.document_id
            )));
        }

        let will_retry = retry_at.is_some() && candidate.attempt_count < candidate.max_attempts;
        let status = if will_retry {
            DocumentJobStatus::RetryWait
        } else {
            DocumentJobStatus::Failed
        };
        let job = state.jobs.get_mut(&id).unwrap();
        job.status = status;
        job.lease_token = None;
        job.lease_expires_at = None;
        job.last_error_code = Some(error_code.to_owned());
        job.last_error_detail = error_detail.map(str::to_owned);
        job.updated_at = failed_at;
        if let Some(retry_at) = retry_at.filter(|_| will_retry) {
            job.available_at = retry_at;
        } else {
            job.finished_at = Some(failed_at);
        }
        state
            .documents
            .get_mut(&candidate.document_id)
            .unwrap()
            .processing_status = if will_retry {
            DocumentProcessingStatus::Queued
        } else {
            DocumentProcessingStatus::Failed
        };
        Ok(Some(status))
    }
    async fn mark_document_indexed(
        &self,
        id: DocumentId,
        revision: i64,
        revision_token: uuid::Uuid,
        fingerprint: &str,
        indexed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        if fingerprint.is_empty()
            || fingerprint.chars().count() > crate::model::DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN
        {
            return Err(AgentError::Store(
                "document index fingerprint must contain 1 to 512 characters".into(),
            ));
        }
        let mut state = self.document_state.lock().unwrap();
        let Some(document) = state.documents.get_mut(&id) else {
            return Ok(false);
        };
        if document.content_revision != revision || document.revision_token != revision_token {
            return Ok(false);
        }
        document.indexed_revision = Some(revision);
        document.index_fingerprint = Some(fingerprint.to_string());
        document.indexed_at = Some(indexed_at);
        document.processing_status = DocumentProcessingStatus::Ready;
        Ok(true)
    }
    async fn clear_document_index(
        &self,
        id: DocumentId,
        revision: i64,
        revision_token: uuid::Uuid,
    ) -> Result<bool> {
        let mut state = self.document_state.lock().unwrap();
        let Some(document) = state.documents.get_mut(&id) else {
            return Ok(false);
        };
        if document.content_revision != revision || document.revision_token != revision_token {
            return Ok(false);
        }
        document.indexed_revision = None;
        document.index_fingerprint = None;
        document.indexed_at = None;
        document.processing_status = DocumentProcessingStatus::Queued;
        Ok(true)
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
    async fn update_chat_metadata(
        &self,
        id: ChatId,
        title: Option<Option<String>>,
        model: Option<Option<String>>,
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
        calls.sort_by_key(|call| (call.created_at, call.id.0));
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
        calls.sort_by_key(|call| call.created_at);
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
}

fn document_summary(document: &DocumentRecord) -> DocumentSummaryRecord {
    DocumentSummaryRecord {
        id: document.id,
        project_id: document.project_id,
        source_uri: document.source_uri.clone(),
        media_type: document.media_type.clone(),
        title: document.title.clone(),
        content_revision: document.content_revision,
        processing_status: document.processing_status,
        indexed_revision: document.indexed_revision,
        index_fingerprint: document.index_fingerprint.clone(),
        created_at: document.created_at,
        updated_at: document.updated_at,
        indexed_at: document.indexed_at,
    }
}

#[test]
fn mem_store_create_document_rejects_an_unknown_project() {
    let store = MemStore::default();
    let now = chrono::Utc::now();
    let document = DocumentRecord {
        id: DocumentId::new(),
        project_id: Some(ProjectId::new()),
        source_uri: None,
        media_type: "text/plain".into(),
        title: None,
        source_blob: None,
        canonical_text: "orphan".into(),
        canonical_fingerprint: None,
        source_regions: Vec::new(),
        content_revision: 1,
        revision_token: uuid::Uuid::new_v4(),
        processing_status: DocumentProcessingStatus::Queued,
        indexed_revision: None,
        index_fingerprint: None,
        created_at: now,
        updated_at: now,
        indexed_at: None,
    };

    assert!(block_on(store.create_document(&document)).is_err());
    assert_eq!(block_on(store.get_document(document.id)).unwrap(), None);
    assert_eq!(
        block_on(store.get_document_generation(document.id)).unwrap(),
        None
    );
}

#[test]
fn store_is_object_safe_and_roundtrips() {
    let store: Arc<dyn Store> = Arc::new(MemStore::default());
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        attachment_revision: 0,
        root_attachments: Vec::new(),
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
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///mem-store.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "atomic source and job".into(),
        source_regions: Vec::new(),
        updated_at: chrono::DateTime::<chrono::Utc>::from_timestamp(1, 0).unwrap(),
    };
    let first =
        block_on(store.upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)).unwrap();
    let retry = DocumentUpsert {
        updated_at: chrono::DateTime::<chrono::Utc>::from_timestamp(2, 0).unwrap(),
        ..source.clone()
    };
    assert_eq!(
        block_on(store.upsert_document_and_enqueue_index(&retry, "pipeline-v1", 3)).unwrap(),
        first
    );
    assert_eq!(
        block_on(store.list_document_jobs(source.id)).unwrap(),
        vec![first.1.clone()]
    );

    let claim_at = first.1.available_at + chrono::Duration::seconds(1);
    let lease_expires_at = claim_at + chrono::Duration::minutes(5);
    let claimed = block_on(store.claim_document_job(claim_at, lease_expires_at))
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, first.1.id);
    let extended = lease_expires_at + chrono::Duration::minutes(5);
    assert!(block_on(store.heartbeat_document_job(
        claimed.id,
        claimed.lease_token.unwrap(),
        claim_at + chrono::Duration::minutes(1),
        extended,
    ))
    .unwrap());
    assert!(block_on(store.complete_document_index_job(
        claimed.id,
        claimed.lease_token.unwrap(),
        claim_at + chrono::Duration::minutes(2),
    ))
    .unwrap());

    let tombstone = block_on(store.delete_document(source.id)).unwrap();
    assert_eq!(tombstone.content_revision, 2);
    assert_eq!(
        block_on(store.delete_document(source.id)).unwrap(),
        tombstone
    );
    assert_eq!(
        block_on(store.get_document_generation(source.id)).unwrap(),
        Some(tombstone)
    );
    assert_eq!(block_on(store.get_document(source.id)).unwrap(), None);
    assert_eq!(block_on(store.get_document_job(first.1.id)).unwrap(), None);

    let retry_source = DocumentUpsert {
        canonical_text: "retry state".into(),
        source_regions: Vec::new(),
        ..source
    };
    let (recreated, retry_job) =
        block_on(store.upsert_document_and_enqueue_index(&retry_source, "pipeline-v1", 2)).unwrap();
    assert_eq!(recreated.content_revision, 3);
    let retry_claim_at = retry_job.available_at + chrono::Duration::seconds(1);
    let retry_claim = block_on(store.claim_document_job(
        retry_claim_at,
        retry_claim_at + chrono::Duration::minutes(5),
    ))
    .unwrap()
    .unwrap();
    assert_eq!(
        block_on(store.record_document_job_failure(
            retry_claim.id,
            retry_claim.lease_token.unwrap(),
            retry_claim_at + chrono::Duration::minutes(1),
            Some(retry_claim_at + chrono::Duration::minutes(2)),
            "timeout",
            None,
        ))
        .unwrap(),
        Some(DocumentJobStatus::RetryWait)
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
        id: DocumentId::new(),
        project_id: Some(project_a.id),
        source_uri: Some("file:///scoped.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "project A source".into(),
        source_regions: Vec::new(),
        updated_at: chrono::Utc::now(),
    };
    let first =
        block_on(store.upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)).unwrap();
    let moved = DocumentUpsert {
        project_id: Some(project_b.id),
        canonical_text: "must not move".into(),
        source_regions: Vec::new(),
        ..source
    };
    assert!(block_on(store.upsert_document_and_enqueue_index(&moved, "pipeline-v1", 3)).is_err());
    assert_eq!(
        block_on(store.get_document(moved.id)).unwrap(),
        Some(first.0)
    );
}

#[test]
fn mem_index_maintenance_requeues_or_advances_by_reason() {
    let store = MemStore::default();
    let source = DocumentUpsert {
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///maintenance.txt".into()),
        media_type: "text/plain".into(),
        title: Some("maintenance".into()),
        canonical_text: "stable source".into(),
        source_regions: Vec::new(),
        updated_at: chrono::DateTime::<chrono::Utc>::from_timestamp(100, 0).unwrap(),
    };
    let (first_document, first_job) =
        block_on(store.upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)).unwrap();
    let claim_at = first_job.available_at + chrono::Duration::seconds(1);
    let claimed =
        block_on(store.claim_document_job(claim_at, claim_at + chrono::Duration::minutes(1)))
            .unwrap()
            .unwrap();
    assert!(block_on(store.complete_document_index_job(
        claimed.id,
        claimed.lease_token.unwrap(),
        claim_at + chrono::Duration::seconds(1),
    ))
    .unwrap());

    let missing = block_on(store.ensure_document_index_job(
        source.id,
        first_document.generation(),
        "pipeline-v1",
        5,
        DocumentIndexJobReason::DerivedStateMissing,
    ))
    .unwrap();
    let EnsureDocumentIndexJobOutcome::Enqueued(requeued) = missing else {
        panic!("missing state should requeue the exact succeeded job")
    };
    assert_eq!(requeued.id, first_job.id);
    assert_eq!(requeued.generation(), first_document.generation());
    assert_eq!(requeued.max_attempts, 5);

    let changed = block_on(store.ensure_document_index_job(
        source.id,
        first_document.generation(),
        "pipeline-v2",
        4,
        DocumentIndexJobReason::PipelineChanged,
    ))
    .unwrap();
    let EnsureDocumentIndexJobOutcome::Enqueued(changed_job) = changed else {
        panic!("pipeline change should enqueue an advanced generation")
    };
    assert_eq!(
        changed_job.content_revision,
        first_document.content_revision + 1
    );
    let repeated = block_on(store.ensure_document_index_job(
        source.id,
        first_document.generation(),
        "pipeline-v2",
        4,
        DocumentIndexJobReason::PipelineChanged,
    ))
    .unwrap();
    assert_eq!(
        repeated,
        EnsureDocumentIndexJobOutcome::Existing(changed_job.clone())
    );
    let changed_claim_at = changed_job.available_at + chrono::Duration::seconds(1);
    let changed_claim = block_on(store.claim_document_job(
        changed_claim_at,
        changed_claim_at + chrono::Duration::minutes(1),
    ))
    .unwrap()
    .unwrap();
    assert_eq!(changed_claim.id, changed_job.id);
    assert!(block_on(store.complete_document_index_job(
        changed_claim.id,
        changed_claim.lease_token.unwrap(),
        changed_claim_at + chrono::Duration::seconds(1),
    ))
    .unwrap());
    let incomplete = block_on(store.ensure_document_index_job(
        source.id,
        changed_job.generation(),
        "pipeline-v2",
        6,
        DocumentIndexJobReason::DerivedStateIncomplete,
    ))
    .unwrap();
    let EnsureDocumentIndexJobOutcome::Enqueued(incomplete_job) = incomplete else {
        panic!("incomplete succeeded state should advance its generation")
    };
    assert_eq!(
        incomplete_job.content_revision,
        changed_job.content_revision + 1
    );
    let current = block_on(store.get_document(source.id)).unwrap().unwrap();
    assert_eq!(current.generation(), incomplete_job.generation());
    assert_eq!(current.canonical_text, source.canonical_text);
    assert_eq!(current.created_at, first_document.created_at);
    assert_eq!(current.updated_at, first_document.updated_at);
}

#[test]
fn mem_store_generation_overflow_leaves_source_job_and_clock_unchanged() {
    let store = MemStore::default();
    let source = DocumentUpsert {
        id: DocumentId::new(),
        project_id: None,
        source_uri: None,
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "maximum generation".into(),
        source_regions: Vec::new(),
        updated_at: chrono::DateTime::<chrono::Utc>::from_timestamp(1, 0).unwrap(),
    };
    let (record, job) =
        block_on(store.upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)).unwrap();
    let maximum = DocumentGeneration {
        content_revision: i64::MAX,
        revision_token: record.revision_token,
    };
    {
        let mut state = store.document_state.lock().unwrap();
        state.generations.insert(source.id, maximum);
        state
            .documents
            .get_mut(&source.id)
            .unwrap()
            .content_revision = i64::MAX;
    }

    assert!(block_on(store.delete_document(source.id)).is_err());
    let retained = block_on(store.get_document(source.id)).unwrap().unwrap();
    assert_eq!(retained.generation(), maximum);
    assert_eq!(
        block_on(store.get_document_generation(source.id)).unwrap(),
        Some(maximum)
    );
    assert_eq!(
        block_on(store.get_document_job(job.id)).unwrap(),
        Some(job.clone())
    );

    let succeeded = {
        let mut state = store.document_state.lock().unwrap();
        let now = chrono::Utc::now();
        let job = state.jobs.get_mut(&job.id).unwrap();
        job.content_revision = i64::MAX;
        job.status = DocumentJobStatus::Succeeded;
        job.attempt_count = 1;
        job.finished_at = Some(now);
        job.updated_at = now;
        job.clone()
    };
    assert!(block_on(store.ensure_document_index_job(
        source.id,
        maximum,
        "pipeline-v1",
        3,
        DocumentIndexJobReason::DerivedStateIncomplete,
    ))
    .is_err());
    assert_eq!(
        block_on(store.get_document(source.id))
            .unwrap()
            .unwrap()
            .generation(),
        maximum
    );
    assert_eq!(
        block_on(store.get_document_generation(source.id)).unwrap(),
        Some(maximum)
    );
    assert_eq!(
        block_on(store.get_document_job(succeeded.id)).unwrap(),
        Some(succeeded)
    );
}

#[test]
fn mem_store_document_retirement_is_durable_state_with_exact_completion() {
    let store = MemStore::default();
    let source = DocumentUpsert {
        id: DocumentId::new(),
        project_id: None,
        source_uri: None,
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "retire me".into(),
        source_regions: Vec::new(),
        updated_at: chrono::DateTime::<chrono::Utc>::from_timestamp(1, 0).unwrap(),
    };
    block_on(store.upsert_document(&source)).unwrap();

    let tombstone = block_on(store.delete_document(source.id)).unwrap();
    assert_eq!(
        block_on(store.list_pending_document_retirements(None, 10)).unwrap(),
        vec![(source.id, tombstone)]
    );
    assert_eq!(
        block_on(store.delete_document(source.id)).unwrap(),
        tombstone
    );

    let recreated = block_on(store.upsert_document(&DocumentUpsert {
        canonical_text: "new lifecycle".into(),
        source_regions: Vec::new(),
        ..source.clone()
    }))
    .unwrap();
    assert_eq!(
        block_on(store.list_pending_document_retirements(None, 10)).unwrap(),
        vec![(source.id, tombstone)]
    );
    assert_eq!(
        block_on(store.get_pending_document_retirement(source.id)).unwrap(),
        Some(tombstone)
    );
    assert!(block_on(store.complete_document_retirement(source.id, tombstone)).unwrap());
    assert!(!block_on(store.complete_document_retirement(source.id, tombstone)).unwrap());

    let current_tombstone = block_on(store.delete_document(source.id)).unwrap();
    assert_ne!(current_tombstone, tombstone);
    assert_eq!(
        current_tombstone.content_revision,
        recreated.content_revision + 1
    );
    assert!(!block_on(store.complete_document_retirement(source.id, tombstone)).unwrap());
    assert!(block_on(store.complete_document_retirement(source.id, current_tombstone)).unwrap());
    assert!(!block_on(store.complete_document_retirement(source.id, current_tombstone)).unwrap());
    assert!(block_on(store.list_pending_document_retirements(None, 10))
        .unwrap()
        .is_empty());
}

#[test]
fn mem_store_pending_retirement_cursor_advances_and_can_wrap() {
    let store = MemStore::default();
    let ids = [1_u128, 2, 3].map(|value| DocumentId(uuid::Uuid::from_u128(value)));
    let generations = ids.map(|id| block_on(store.delete_document(id)).unwrap());

    assert_eq!(
        block_on(store.list_pending_document_retirements(None, 2)).unwrap(),
        vec![(ids[0], generations[0]), (ids[1], generations[1])]
    );
    assert_eq!(
        block_on(store.list_pending_document_retirements(Some(ids[1]), 2)).unwrap(),
        vec![(ids[2], generations[2])]
    );
    assert!(
        block_on(store.list_pending_document_retirements(Some(ids[2]), 2))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        block_on(store.list_pending_document_retirements(None, 1)).unwrap(),
        vec![(ids[0], generations[0])]
    );
}

#[test]
fn mem_store_ensure_parse_job_advances_parser_changes_once() {
    let store = MemStore::default();
    let now = chrono::Utc::now();
    let document_id = DocumentId::new();
    let generation = DocumentGeneration {
        content_revision: 1,
        revision_token: uuid::Uuid::new_v4(),
    };
    let index_job = DocumentJob {
        id: DocumentJobId::new(),
        document_id,
        content_revision: generation.content_revision,
        revision_token: generation.revision_token,
        kind: DocumentJobKind::Index,
        status: DocumentJobStatus::Queued,
        pipeline_fingerprint: "index-v1".into(),
        attempt_count: 0,
        max_attempts: 3,
        available_at: now,
        lease_token: None,
        lease_expires_at: None,
        started_at: None,
        finished_at: None,
        last_error_code: None,
        last_error_detail: None,
        created_at: now,
        updated_at: now,
    };
    {
        let mut state = store.document_state.lock().unwrap();
        state.generations.insert(document_id, generation);
        state.documents.insert(
            document_id,
            DocumentRecord {
                id: document_id,
                project_id: None,
                source_uri: Some("file:///parser-upgrade.txt".into()),
                media_type: "text/plain".into(),
                title: None,
                source_blob: Some(crate::model::DocumentSourceBlob {
                    id: uuid::Uuid::new_v4(),
                    sha256: [0x22; 32],
                    byte_len: 128,
                }),
                canonical_text: "canonical v1".into(),
                canonical_fingerprint: Some("parser-v1".into()),
                source_regions: Vec::new(),
                content_revision: generation.content_revision,
                revision_token: generation.revision_token,
                processing_status: DocumentProcessingStatus::Queued,
                indexed_revision: None,
                index_fingerprint: None,
                created_at: now,
                updated_at: now,
                indexed_at: None,
            },
        );
        state.jobs.insert(index_job.id, index_job.clone());
    }

    let outcome =
        block_on(store.ensure_document_parse_job(document_id, generation, "parser-v2", 4)).unwrap();
    let EnsureDocumentParseJobOutcome::Enqueued(reparse_job) = outcome else {
        panic!("expected a parser-change reparse job, got {outcome:?}");
    };
    assert_eq!(reparse_job.content_revision, 2);
    assert_eq!(reparse_job.kind, DocumentJobKind::Parse);
    assert_eq!(
        block_on(store.get_document_job(index_job.id))
            .unwrap()
            .unwrap()
            .status,
        DocumentJobStatus::Cancelled
    );
    let reparsing = block_on(store.get_document(document_id)).unwrap().unwrap();
    assert_eq!(reparsing.generation(), reparse_job.generation());
    assert!(reparsing.canonical_text.is_empty());
    assert_eq!(reparsing.canonical_fingerprint, None);
    assert_eq!(
        reparsing.processing_status,
        DocumentProcessingStatus::Queued
    );
    assert_eq!(
        block_on(store.ensure_document_parse_job(document_id, generation, "parser-v2", 8,))
            .unwrap(),
        EnsureDocumentParseJobOutcome::GenerationChanged(reparse_job.generation())
    );
    assert_eq!(
        block_on(store.ensure_document_parse_job(
            document_id,
            reparse_job.generation(),
            "parser-v2",
            8,
        ))
        .unwrap(),
        EnsureDocumentParseJobOutcome::Existing(reparse_job)
    );
}

#[test]
fn mem_store_explicit_retry_only_revives_current_failed_index_job() {
    let store = MemStore::default();
    let source = DocumentUpsert {
        id: DocumentId::new(),
        project_id: None,
        source_uri: None,
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "retry me".into(),
        source_regions: Vec::new(),
        updated_at: chrono::DateTime::<chrono::Utc>::from_timestamp(1, 0).unwrap(),
    };
    let (document, queued) =
        block_on(store.upsert_document_and_enqueue_index(&source, "pipeline-v1", 2)).unwrap();
    assert_eq!(
        block_on(store.retry_document_job(
            source.id,
            document.generation(),
            DocumentJobKind::Index,
            "pipeline-v1",
            9,
        ))
        .unwrap(),
        Some(queued.clone())
    );

    let claim_at = queued.available_at + chrono::Duration::seconds(1);
    let running =
        block_on(store.claim_document_job(claim_at, claim_at + chrono::Duration::minutes(1)))
            .unwrap()
            .unwrap();
    assert_eq!(
        block_on(store.retry_document_job(
            source.id,
            document.generation(),
            DocumentJobKind::Index,
            "pipeline-v1",
            9,
        ))
        .unwrap(),
        Some(running.clone())
    );
    assert_eq!(
        block_on(store.record_document_job_failure(
            running.id,
            running.lease_token.unwrap(),
            claim_at + chrono::Duration::seconds(1),
            None,
            "embedding_failed",
            Some("service unavailable"),
        ))
        .unwrap(),
        Some(DocumentJobStatus::Failed)
    );
    assert_eq!(
        block_on(store.retry_document_job(
            source.id,
            document.generation(),
            DocumentJobKind::Index,
            "other-pipeline",
            4,
        ))
        .unwrap(),
        None
    );
    assert_eq!(
        block_on(store.retry_document_job(
            source.id,
            DocumentGeneration {
                content_revision: document.content_revision + 1,
                revision_token: document.revision_token,
            },
            DocumentJobKind::Index,
            "pipeline-v1",
            4,
        ))
        .unwrap(),
        None
    );

    let retried = block_on(store.retry_document_job(
        source.id,
        document.generation(),
        DocumentJobKind::Index,
        "pipeline-v1",
        4,
    ))
    .unwrap()
    .unwrap();
    assert_eq!(retried.id, queued.id);
    assert_eq!(retried.status, DocumentJobStatus::Queued);
    assert_eq!(retried.attempt_count, 0);
    assert_eq!(retried.max_attempts, 4);
    assert_eq!(retried.lease_token, None);
    assert_eq!(retried.lease_expires_at, None);
    assert_eq!(retried.started_at, None);
    assert_eq!(retried.finished_at, None);
    assert_eq!(retried.last_error_code, None);
    assert_eq!(retried.last_error_detail, None);
    let document = block_on(store.get_document(source.id)).unwrap().unwrap();
    assert_eq!(document.processing_status, DocumentProcessingStatus::Queued);
    assert_eq!(document.indexed_revision, None);
    assert_eq!(document.index_fingerprint, None);
    assert_eq!(document.indexed_at, None);
    assert_eq!(
        block_on(store.retry_document_job(
            source.id,
            document.generation(),
            DocumentJobKind::Index,
            "pipeline-v1",
            8,
        ))
        .unwrap(),
        Some(retried.clone())
    );

    let retry_claim_at = retried.available_at + chrono::Duration::seconds(1);
    let retry_running = block_on(store.claim_document_job(
        retry_claim_at,
        retry_claim_at + chrono::Duration::minutes(1),
    ))
    .unwrap()
    .unwrap();
    assert!(block_on(store.complete_document_index_job(
        retry_running.id,
        retry_running.lease_token.unwrap(),
        retry_claim_at + chrono::Duration::seconds(1),
    ))
    .unwrap());
    assert_eq!(
        block_on(store.retry_document_job(
            source.id,
            document.generation(),
            DocumentJobKind::Index,
            "pipeline-v1",
            4,
        ))
        .unwrap(),
        None
    );

    let replacement = DocumentUpsert {
        canonical_text: "replacement".into(),
        source_regions: Vec::new(),
        updated_at: source.updated_at + chrono::Duration::seconds(1),
        ..source.clone()
    };
    let (replacement_document, cancelled) =
        block_on(store.upsert_document_and_enqueue_index(&replacement, "pipeline-v2", 2)).unwrap();
    assert_eq!(
        block_on(store.retry_document_job(
            replacement.id,
            replacement_document.generation(),
            DocumentJobKind::Index,
            "pipeline-v1",
            4,
        ))
        .unwrap(),
        None
    );
    let (newer_document, _) = block_on(store.upsert_document_and_enqueue_index(
        &DocumentUpsert {
            canonical_text: "newer replacement".into(),
            source_regions: Vec::new(),
            updated_at: source.updated_at + chrono::Duration::seconds(2),
            ..replacement
        },
        "pipeline-v3",
        2,
    ))
    .unwrap();
    assert_eq!(
        block_on(store.get_document_job(cancelled.id))
            .unwrap()
            .unwrap()
            .status,
        DocumentJobStatus::Cancelled
    );
    assert_eq!(
        block_on(store.retry_document_job(
            source.id,
            newer_document.generation(),
            DocumentJobKind::Index,
            "pipeline-v2",
            4,
        ))
        .unwrap(),
        None
    );
}

#[test]
fn mem_store_explicit_retry_revives_only_pending_parse_stage() {
    let store = MemStore::default();
    let now = chrono::Utc::now();
    let document_id = DocumentId::new();
    let generation = DocumentGeneration {
        content_revision: 1,
        revision_token: uuid::Uuid::new_v4(),
    };
    let job = DocumentJob {
        id: DocumentJobId::new(),
        document_id,
        content_revision: generation.content_revision,
        revision_token: generation.revision_token,
        kind: DocumentJobKind::Parse,
        status: DocumentJobStatus::Failed,
        pipeline_fingerprint: "parser=pdf-v1".into(),
        attempt_count: 3,
        max_attempts: 3,
        available_at: now,
        lease_token: None,
        lease_expires_at: None,
        started_at: Some(now),
        finished_at: Some(now),
        last_error_code: Some("parse_failed".into()),
        last_error_detail: Some("malformed page".into()),
        created_at: now,
        updated_at: now,
    };
    {
        let mut state = store.document_state.lock().unwrap();
        state.documents.insert(
            document_id,
            DocumentRecord {
                id: document_id,
                project_id: None,
                source_uri: Some("file:///report.pdf".into()),
                media_type: "application/pdf".into(),
                title: None,
                source_blob: Some(crate::model::DocumentSourceBlob {
                    id: uuid::Uuid::new_v4(),
                    sha256: [0x33; 32],
                    byte_len: 4_096,
                }),
                canonical_text: String::new(),
                canonical_fingerprint: None,
                source_regions: Vec::new(),
                content_revision: generation.content_revision,
                revision_token: generation.revision_token,
                processing_status: DocumentProcessingStatus::Failed,
                indexed_revision: None,
                index_fingerprint: None,
                created_at: now,
                updated_at: now,
                indexed_at: None,
            },
        );
        state.jobs.insert(job.id, job.clone());
    }

    assert_eq!(
        block_on(store.ensure_document_parse_job(document_id, generation, "parser=pdf-v1", 5,))
            .unwrap(),
        EnsureDocumentParseJobOutcome::Failed(job.clone())
    );

    assert_eq!(
        block_on(store.retry_document_job(
            document_id,
            generation,
            DocumentJobKind::Index,
            "parser=pdf-v1",
            5,
        ))
        .unwrap(),
        None
    );
    let retried = block_on(store.retry_document_job(
        document_id,
        generation,
        DocumentJobKind::Parse,
        "parser=pdf-v1",
        5,
    ))
    .unwrap()
    .unwrap();
    assert_eq!(retried.id, job.id);
    assert_eq!(retried.kind, DocumentJobKind::Parse);
    assert_eq!(retried.status, DocumentJobStatus::Queued);
    assert_eq!(retried.attempt_count, 0);
    assert_eq!(retried.max_attempts, 5);
    assert_eq!(retried.started_at, None);
    assert_eq!(retried.finished_at, None);
    assert_eq!(retried.last_error_code, None);
    assert_eq!(retried.last_error_detail, None);
    assert_eq!(
        block_on(store.get_document(document_id))
            .unwrap()
            .unwrap()
            .processing_status,
        DocumentProcessingStatus::Queued
    );
}
