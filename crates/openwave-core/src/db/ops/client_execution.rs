use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::id::{CallId, ChatId};
use crate::model::{
    RetrievalEvidence, RetrievalEvidenceInput, RetrievalEvidenceSource, ToolCallExecution,
    ToolCallRecord, ToolCallResolution, ToolCallStatus,
};
use crate::storage::{
    AcceptToolCallOutcome, ClaimClientToolCallOutcome, ClientToolCallClaim,
    HeartbeatClientToolCallOutcome, JournaledClientToolCallOutcome, ResolveToolCallOutcome,
};

use super::super::{entities, store_err, DbStore};
use super::turn::canonical_db_timestamp;
use super::{acquire_chat_write_lock, acquire_tool_call_write_lock};

pub(in crate::db) async fn accept_tool_call(
    store: &DbStore,
    call: &ToolCallRecord,
) -> Result<AcceptToolCallOutcome> {
    validate_accept(call)?;
    let created_at = canonical_db_timestamp(call.created_at)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, call.chat_id).await? {
        return Err(AgentError::Store(format!(
            "chat {} does not exist",
            call.chat_id
        )));
    }
    if let Some(existing) = entities::tool_call::Entity::find_by_id(call.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let outcome = if immutable_request_matches(&existing, call, created_at) {
            AcceptToolCallOutcome::Existing(tool_call_from_model(existing)?)
        } else {
            AcceptToolCallOutcome::IdentityConflict
        };
        transaction.commit().await.map_err(store_err)?;
        return Ok(outcome);
    }

    let inserted = entities::tool_call::ActiveModel {
        id: Set(call.id.0),
        chat_id: Set(call.chat_id.0),
        turn_id: Set(call.turn_id.0),
        provider_id: Set(call.provider_id.clone()),
        name: Set(call.name.clone()),
        arguments: Set(call.arguments.clone()),
        execution: Set(call.execution.as_str().into()),
        status: Set(ToolCallStatus::Pending.as_str().into()),
        result: Set(None),
        error_code: Set(None),
        error_detail: Set(None),
        approval_status: Set(None),
        approval_class: Set(None),
        approval_kind: Set(None),
        approval_reason: Set(None),
        approval_requested_at: Set(None),
        approval_decided_at: Set(None),
        approval_event_seq: Set(None),
        client_executor_id: Set(None),
        client_lease_token: Set(None),
        client_lease_expires_at: Set(None),
        created_at: Set(created_at),
        resolved_at: Set(None),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    let inserted = tool_call_from_model(inserted)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(AcceptToolCallOutcome::Accepted(inserted))
}

pub(in crate::db) async fn claim_client_tool_call(
    store: &DbStore,
    id: CallId,
    chat_id: ChatId,
    executor_id: uuid::Uuid,
    lease_token: uuid::Uuid,
    now: DateTime<Utc>,
    lease_expires_at: DateTime<Utc>,
) -> Result<ClaimClientToolCallOutcome> {
    let now = canonical_db_timestamp(now)?;
    let lease_expires_at = canonical_db_timestamp(lease_expires_at)?;
    if executor_id.is_nil() || lease_token.is_nil() {
        return Err(AgentError::Store(
            "client executor id and lease token must not be nil".into(),
        ));
    }
    if lease_expires_at <= now {
        return Ok(ClaimClientToolCallOutcome::Unavailable);
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimClientToolCallOutcome::Unavailable);
    }
    if !acquire_tool_call_write_lock(&transaction, id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimClientToolCallOutcome::Unavailable);
    }
    let existing = entities::tool_call::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked tool call exists");
    if existing.chat_id != chat_id.0
        || existing.execution != ToolCallExecution::Client.as_str()
        || existing.status != ToolCallStatus::Pending.as_str()
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimClientToolCallOutcome::Unavailable);
    }
    if existing.client_executor_id == Some(executor_id)
        && existing.client_lease_token == Some(lease_token)
        && existing
            .client_lease_expires_at
            .is_some_and(|expiry| expiry > now)
    {
        let claim = client_claim_from_model(existing)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimClientToolCallOutcome::Existing(claim));
    }
    if existing.client_executor_id.is_some() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimClientToolCallOutcome::Unavailable);
    }

    let mut active: entities::tool_call::ActiveModel = existing.into();
    active.client_executor_id = Set(Some(executor_id));
    active.client_lease_token = Set(Some(lease_token));
    active.client_lease_expires_at = Set(Some(lease_expires_at));
    let claimed = active.update(&transaction).await.map_err(store_err)?;
    let claim = client_claim_from_model(claimed)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(ClaimClientToolCallOutcome::Claimed(claim))
}

pub(in crate::db) async fn heartbeat_client_tool_call(
    store: &DbStore,
    id: CallId,
    chat_id: ChatId,
    lease_token: uuid::Uuid,
    now: DateTime<Utc>,
    lease_expires_at: DateTime<Utc>,
) -> Result<HeartbeatClientToolCallOutcome> {
    let now = canonical_db_timestamp(now)?;
    let lease_expires_at = canonical_db_timestamp(lease_expires_at)?;
    validate_lease(lease_token, now, lease_expires_at)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(HeartbeatClientToolCallOutcome::LeaseLost);
    }
    if !acquire_tool_call_write_lock(&transaction, id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(HeartbeatClientToolCallOutcome::LeaseLost);
    }
    let existing = entities::tool_call::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked tool call exists");
    let current_expiry = existing.client_lease_expires_at;
    if existing.chat_id != chat_id.0
        || existing.execution != ToolCallExecution::Client.as_str()
        || existing.status != ToolCallStatus::Pending.as_str()
        || existing.client_lease_token != Some(lease_token)
        || current_expiry.is_none_or(|expiry| expiry <= now || lease_expires_at < expiry)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(HeartbeatClientToolCallOutcome::LeaseLost);
    }
    if current_expiry == Some(lease_expires_at) {
        transaction.commit().await.map_err(store_err)?;
        return Ok(HeartbeatClientToolCallOutcome::Existing);
    }
    let mut active: entities::tool_call::ActiveModel = existing.into();
    active.client_lease_expires_at = Set(Some(lease_expires_at));
    active.update(&transaction).await.map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(HeartbeatClientToolCallOutcome::Extended)
}

pub(in crate::db) async fn resolve_server_tool_call(
    store: &DbStore,
    id: CallId,
    resolution: &ToolCallResolution,
    resolved_at: DateTime<Utc>,
) -> Result<ResolveToolCallOutcome> {
    Ok(resolve_tool_call(
        store,
        id,
        ResolutionAuthority::Server,
        resolved_at,
        resolution,
        None,
    )
    .await?
    .outcome)
}

pub(in crate::db) async fn resolve_server_tool_call_with_evidence(
    store: &DbStore,
    id: CallId,
    resolution: &ToolCallResolution,
    resolved_at: DateTime<Utc>,
    evidence: &[RetrievalEvidenceInput],
) -> Result<ResolveToolCallOutcome> {
    Ok(resolve_tool_call(
        store,
        id,
        ResolutionAuthority::Server,
        resolved_at,
        resolution,
        Some(evidence),
    )
    .await?
    .outcome)
}

pub(in crate::db) async fn list_retrieval_evidence(
    store: &DbStore,
    id: CallId,
) -> Result<Vec<RetrievalEvidence>> {
    evidence_models(&store.conn, id)
        .await?
        .into_iter()
        .map(evidence_from_model)
        .collect()
}

pub(in crate::db) async fn resolve_client_tool_call_and_append_event(
    store: &DbStore,
    id: CallId,
    chat_id: ChatId,
    lease_token: uuid::Uuid,
    now: DateTime<Utc>,
    resolution: &ToolCallResolution,
    resolved_at: DateTime<Utc>,
) -> Result<JournaledClientToolCallOutcome> {
    if lease_token.is_nil() {
        return Err(AgentError::Store(
            "client lease token must not be nil".into(),
        ));
    }
    resolve_tool_call(
        store,
        id,
        ResolutionAuthority::LiveClient {
            chat_id,
            lease_token,
            now,
        },
        resolved_at,
        resolution,
        None,
    )
    .await
}

pub(in crate::db) async fn resolve_expired_client_tool_call_and_append_event(
    store: &DbStore,
    id: CallId,
    chat_id: ChatId,
    lease_token: uuid::Uuid,
    now: DateTime<Utc>,
    resolution: &ToolCallResolution,
    resolved_at: DateTime<Utc>,
) -> Result<JournaledClientToolCallOutcome> {
    if lease_token.is_nil() {
        return Err(AgentError::Store(
            "client lease token must not be nil".into(),
        ));
    }
    resolve_tool_call(
        store,
        id,
        ResolutionAuthority::ExpiredClient {
            chat_id,
            lease_token,
            now,
        },
        resolved_at,
        resolution,
        None,
    )
    .await
}

pub(in crate::db) async fn list_pending_client_tool_calls(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<ToolCallRecord>> {
    let models = entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::ChatId.eq(chat_id.0))
        .filter(entities::tool_call::Column::Execution.eq(ToolCallExecution::Client.as_str()))
        .filter(entities::tool_call::Column::Status.eq(ToolCallStatus::Pending.as_str()))
        .order_by_asc(entities::tool_call::Column::CreatedAt)
        .order_by_asc(entities::tool_call::Column::Id)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    models.into_iter().map(tool_call_from_model).collect()
}

async fn resolve_tool_call(
    store: &DbStore,
    id: CallId,
    authority: ResolutionAuthority,
    resolved_at: DateTime<Utc>,
    resolution: &ToolCallResolution,
    evidence: Option<&[RetrievalEvidenceInput]>,
) -> Result<JournaledClientToolCallOutcome> {
    validate_resolution(resolution)?;
    let resolved_at = canonical_db_timestamp(resolved_at)?;
    let authority = authority.canonicalized()?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if let Some(chat_id) = authority.chat_id() {
        if !acquire_chat_write_lock(&transaction, chat_id).await? {
            transaction.commit().await.map_err(store_err)?;
            return Ok(journaled_call_outcome(ResolveToolCallOutcome::LeaseLost));
        }
    }
    if !acquire_tool_call_write_lock(&transaction, id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(journaled_call_outcome(ResolveToolCallOutcome::NotFound));
    }
    let existing = entities::tool_call::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked tool call exists");
    if let Some(evidence) = evidence {
        validate_evidence_request(&existing, resolution, evidence)?;
    }
    if resolved_at < existing.created_at {
        transaction.commit().await.map_err(store_err)?;
        return Err(AgentError::Store(
            "tool call cannot resolve before it was created".into(),
        ));
    }
    if existing.status != ToolCallStatus::Pending.as_str() {
        let mut outcome = if !terminal_authority_matches(&existing, authority) {
            ResolveToolCallOutcome::LeaseLost
        } else if terminal_payload_matches(&existing, resolution) {
            ResolveToolCallOutcome::Existing
        } else {
            ResolveToolCallOutcome::AlreadyTerminal
        };
        if outcome == ResolveToolCallOutcome::Existing {
            if let Some(expected) = evidence {
                let stored = evidence_models(&transaction, id).await?;
                if !evidence_models_match(&stored, &existing, expected)? {
                    outcome = ResolveToolCallOutcome::AlreadyTerminal;
                }
            }
        }
        let transition = if outcome == ResolveToolCallOutcome::Existing
            && authority.chat_id().is_some()
        {
            super::turn::recover_turn_after_client_resolution_on(&transaction, &existing).await?
        } else {
            None
        };
        transaction.commit().await.map_err(store_err)?;
        return Ok(JournaledClientToolCallOutcome {
            outcome,
            turn: transition.as_ref().map(|item| item.turn.clone()),
            terminal_event: transition.and_then(|item| item.terminal_event),
        });
    }

    let owns = match authority {
        ResolutionAuthority::Server => existing.execution == ToolCallExecution::Server.as_str(),
        ResolutionAuthority::LiveClient {
            chat_id,
            lease_token,
            now,
        } => {
            existing.chat_id == chat_id.0
                && existing.execution == ToolCallExecution::Client.as_str()
                && existing.client_lease_token == Some(lease_token)
                && existing
                    .client_lease_expires_at
                    .is_some_and(|expiry| expiry > now)
        }
        ResolutionAuthority::ExpiredClient {
            chat_id,
            lease_token,
            now,
        } => {
            existing.chat_id == chat_id.0
                && existing.execution == ToolCallExecution::Client.as_str()
                && existing.client_lease_token == Some(lease_token)
                && existing
                    .client_lease_expires_at
                    .is_some_and(|expiry| expiry <= now)
        }
    };
    if !owns {
        transaction.commit().await.map_err(store_err)?;
        return Ok(journaled_call_outcome(ResolveToolCallOutcome::LeaseLost));
    }

    if let Some(evidence) = evidence {
        insert_evidence(&transaction, &existing, evidence).await?;
    }

    let (error_code, error_detail) = resolution_error(resolution);
    let approval_status = existing.approval_status.clone();
    let approval_requested_at = existing.approval_requested_at;
    let mut active: entities::tool_call::ActiveModel = existing.into();
    active.status = Set(resolution.status().as_str().into());
    active.result = Set(Some(resolution.result().to_owned()));
    active.error_code = Set(error_code);
    active.error_detail = Set(error_detail);
    active.client_lease_expires_at = Set(None);
    active.resolved_at = Set(Some(resolved_at));
    if approval_status.as_deref() == Some(crate::ToolApprovalStatus::Pending.as_str()) {
        let requested_at = approval_requested_at
            .ok_or_else(|| AgentError::Store("pending approval is missing requested_at".into()))?;
        active.approval_status = Set(Some(crate::ToolApprovalStatus::Rejected.as_str().into()));
        active.approval_reason = Set(Some("tool ended before approval".into()));
        active.approval_decided_at = Set(Some(resolved_at.max(requested_at)));
    }
    let resolved = active.update(&transaction).await.map_err(store_err)?;
    let transition = if authority.chat_id().is_some() {
        super::turn::advance_turn_after_client_resolution_on(&transaction, &resolved, resolved_at)
            .await?
    } else {
        None
    };
    transaction.commit().await.map_err(store_err)?;
    Ok(JournaledClientToolCallOutcome {
        outcome: ResolveToolCallOutcome::Resolved,
        turn: transition.as_ref().map(|item| item.turn.clone()),
        terminal_event: transition.and_then(|item| item.terminal_event),
    })
}

fn journaled_call_outcome(outcome: ResolveToolCallOutcome) -> JournaledClientToolCallOutcome {
    JournaledClientToolCallOutcome {
        outcome,
        turn: None,
        terminal_event: None,
    }
}

fn validate_evidence_request(
    call: &entities::tool_call::Model,
    resolution: &ToolCallResolution,
    evidence: &[RetrievalEvidenceInput],
) -> Result<()> {
    if evidence.is_empty() {
        return Ok(());
    }
    if call.execution != ToolCallExecution::Server.as_str()
        || call.name != "search"
        || !matches!(resolution, ToolCallResolution::Completed { .. })
        || evidence.len() > RetrievalEvidenceInput::MAX_RESULTS
    {
        return Err(AgentError::Store("invalid retrieval evidence owner".into()));
    }
    let mut source_tokens = std::collections::HashSet::with_capacity(evidence.len());
    for (index, item) in evidence.iter().enumerate() {
        let expected_rank = u16::try_from(index + 1)
            .map_err(|_| AgentError::Store("retrieval evidence rank exhausted".into()))?;
        validate_evidence_item(item, expected_rank)?;
        if !source_tokens.insert(item.source_token) {
            return Err(AgentError::Store(
                "retrieval evidence source tokens must be unique".into(),
            ));
        }
    }
    Ok(())
}

fn validate_evidence_item(item: &RetrievalEvidenceInput, expected_rank: u16) -> Result<()> {
    let heading_bytes = item
        .heading_path
        .iter()
        .try_fold(0_usize, |total, heading| total.checked_add(heading.len()));
    let source_uri_valid = match &item.source {
        RetrievalEvidenceSource::Uri { uri } => {
            !uri.is_empty()
                && uri.len() <= RetrievalEvidenceInput::MAX_SOURCE_URI_BYTES
                && !uri.contains('\0')
        }
        RetrievalEvidenceSource::Inline => true,
    };
    if item.rank != expected_rank
        || item.rank == 0
        || usize::from(item.rank) > RetrievalEvidenceInput::MAX_RESULTS
        || item.source_token.is_nil()
        || item.generation.content_revision < 1
        || item.generation.revision_token.is_nil()
        || item.span.is_empty()
        || i64::try_from(item.span.start).is_err()
        || i64::try_from(item.span.end).is_err()
        || item.span.len() != item.snippet.len()
        || item.snippet.contains('\0')
        || item.snippet.len() > RetrievalEvidenceInput::MAX_SNIPPET_BYTES
        || item.heading_path.len() > RetrievalEvidenceInput::MAX_HEADING_SEGMENTS
        || heading_bytes.is_none_or(|bytes| bytes > RetrievalEvidenceInput::MAX_HEADING_BYTES)
        || item
            .heading_path
            .iter()
            .any(|heading| heading.contains('\0'))
        || item.source_regions.len() > RetrievalEvidenceInput::MAX_SOURCE_REGIONS
        || item.chunk_id != crate::ChunkId::derive(item.document_id, item.span.start, item.span.end)
        || !source_uri_valid
    {
        return Err(AgentError::Store("invalid retrieval evidence".into()));
    }
    let mut previous_end = item.span.start;
    for region in &item.source_regions {
        if region.span.is_empty()
            || region.span.start < item.span.start
            || region.span.end > item.span.end
            || region.span.start < previous_end
            || !item
                .snippet
                .is_char_boundary(region.span.start - item.span.start)
            || !item
                .snippet
                .is_char_boundary(region.span.end - item.span.start)
        {
            return Err(AgentError::Store(
                "invalid retrieval evidence source regions".into(),
            ));
        }
        previous_end = region.span.end;
    }
    Ok(())
}

async fn evidence_models<C>(
    conn: &C,
    id: CallId,
) -> Result<Vec<entities::retrieval_evidence::Model>>
where
    C: ConnectionTrait,
{
    entities::retrieval_evidence::Entity::find()
        .filter(entities::retrieval_evidence::Column::CallId.eq(id.0))
        .order_by_asc(entities::retrieval_evidence::Column::Rank)
        .all(conn)
        .await
        .map_err(store_err)
}

async fn insert_evidence<C>(
    conn: &C,
    call: &entities::tool_call::Model,
    evidence: &[RetrievalEvidenceInput],
) -> Result<()>
where
    C: ConnectionTrait,
{
    if evidence.is_empty() {
        return Ok(());
    }
    let models = evidence
        .iter()
        .map(|item| {
            let (source_kind, source_uri) = match &item.source {
                RetrievalEvidenceSource::Uri { uri } => ("uri", Some(uri.clone())),
                RetrievalEvidenceSource::Inline => ("inline", None),
            };
            Ok(entities::retrieval_evidence::ActiveModel {
                call_id: Set(call.id),
                rank: Set(i32::from(item.rank)),
                source_token: Set(item.source_token),
                chat_id: Set(call.chat_id),
                turn_id: Set(call.turn_id),
                document_id: Set(item.document_id.0),
                content_revision: Set(item.generation.content_revision),
                revision_token: Set(item.generation.revision_token),
                chunk_id: Set(item.chunk_id.0),
                span_start: Set(i64::try_from(item.span.start).map_err(|_| {
                    AgentError::Store("retrieval evidence span exceeds storage range".into())
                })?),
                span_end: Set(i64::try_from(item.span.end).map_err(|_| {
                    AgentError::Store("retrieval evidence span exceeds storage range".into())
                })?),
                snippet: Set(item.snippet.clone()),
                heading_path: Set(serde_json::to_value(&item.heading_path)?),
                source_regions: Set(serde_json::to_value(&item.source_regions)?),
                source_kind: Set(source_kind.into()),
                source_uri: Set(source_uri),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    entities::retrieval_evidence::Entity::insert_many(models)
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

pub(in crate::db) fn evidence_from_model(
    model: entities::retrieval_evidence::Model,
) -> Result<RetrievalEvidence> {
    let span_start = usize::try_from(model.span_start)
        .map_err(|_| AgentError::Store("invalid stored evidence span".into()))?;
    let span_end = usize::try_from(model.span_end)
        .map_err(|_| AgentError::Store("invalid stored evidence span".into()))?;
    if span_start >= span_end {
        return Err(AgentError::Store("invalid stored evidence span".into()));
    }
    let evidence = RetrievalEvidenceInput {
        rank: u16::try_from(model.rank)
            .map_err(|_| AgentError::Store("invalid stored evidence rank".into()))?,
        source_token: model.source_token,
        document_id: model.document_id.into(),
        generation: crate::DocumentGeneration {
            content_revision: model.content_revision,
            revision_token: model.revision_token,
        },
        chunk_id: model.chunk_id.into(),
        span: crate::ByteSpan::new(span_start, span_end),
        snippet: model.snippet,
        heading_path: serde_json::from_value(model.heading_path)?,
        source_regions: serde_json::from_value(model.source_regions)?,
        source: match (model.source_kind.as_str(), model.source_uri) {
            ("uri", Some(uri)) => RetrievalEvidenceSource::Uri { uri },
            ("inline", None) => RetrievalEvidenceSource::Inline,
            _ => return Err(AgentError::Store("invalid stored evidence source".into())),
        },
    };
    validate_evidence_item(&evidence, evidence.rank)?;
    Ok(RetrievalEvidence {
        call_id: model.call_id.into(),
        chat_id: model.chat_id.into(),
        turn_id: model.turn_id.into(),
        evidence,
    })
}

fn evidence_models_match(
    stored: &[entities::retrieval_evidence::Model],
    call: &entities::tool_call::Model,
    expected: &[RetrievalEvidenceInput],
) -> Result<bool> {
    let stored = stored
        .iter()
        .cloned()
        .map(evidence_from_model)
        .collect::<Result<Vec<_>>>()?;
    Ok(stored.len() == expected.len()
        && stored.iter().zip(expected).all(|(stored, expected)| {
            stored.call_id.0 == call.id
                && stored.chat_id.0 == call.chat_id
                && stored.turn_id.0 == call.turn_id
                && &stored.evidence == expected
        }))
}

#[derive(Clone, Copy)]
enum ResolutionAuthority {
    Server,
    LiveClient {
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: DateTime<Utc>,
    },
    ExpiredClient {
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: DateTime<Utc>,
    },
}

impl ResolutionAuthority {
    fn canonicalized(self) -> Result<Self> {
        Ok(match self {
            Self::Server => Self::Server,
            Self::LiveClient {
                chat_id,
                lease_token,
                now,
            } => Self::LiveClient {
                chat_id,
                lease_token,
                now: canonical_db_timestamp(now)?,
            },
            Self::ExpiredClient {
                chat_id,
                lease_token,
                now,
            } => Self::ExpiredClient {
                chat_id,
                lease_token,
                now: canonical_db_timestamp(now)?,
            },
        })
    }

    const fn lease_token(self) -> Option<uuid::Uuid> {
        match self {
            Self::Server => None,
            Self::LiveClient { lease_token, .. } | Self::ExpiredClient { lease_token, .. } => {
                Some(lease_token)
            }
        }
    }

    const fn chat_id(self) -> Option<ChatId> {
        match self {
            Self::Server => None,
            Self::LiveClient { chat_id, .. } | Self::ExpiredClient { chat_id, .. } => Some(chat_id),
        }
    }
}

fn validate_accept(call: &ToolCallRecord) -> Result<()> {
    let labels_valid = [call.provider_id.as_str(), call.name.as_str()]
        .into_iter()
        .all(|value| {
            !value.is_empty()
                && value.len() <= ToolCallRecord::MAX_LABEL_LEN
                && !value.contains('\0')
        });
    let args_len = serde_json::to_vec(&call.arguments)
        .map_err(|error| AgentError::Store(format!("serialize tool arguments: {error}")))?
        .len();
    if call.id.0.is_nil()
        || !labels_valid
        || args_len > ToolCallRecord::MAX_ARGUMENT_BYTES
        || call.status != ToolCallStatus::Pending
        || call.result.is_some()
        || call.error_code.is_some()
        || call.error_detail.is_some()
        || call.client_executor_id.is_some()
        || call.client_lease_expires_at.is_some()
        || call.resolved_at.is_some()
    {
        return Err(AgentError::Store("invalid accepted tool call".into()));
    }
    Ok(())
}

fn validate_lease(
    lease_token: uuid::Uuid,
    now: DateTime<Utc>,
    lease_expires_at: DateTime<Utc>,
) -> Result<()> {
    if lease_token.is_nil() || lease_expires_at <= now {
        return Err(AgentError::Store("invalid client execution lease".into()));
    }
    Ok(())
}

fn validate_resolution(resolution: &ToolCallResolution) -> Result<()> {
    let (error_code, error_detail) = resolution_error(resolution);
    if resolution.result().len() > ToolCallRecord::MAX_RESULT_BYTES
        || resolution.result().contains('\0')
        || error_code.as_deref().is_some_and(|code| {
            code.is_empty()
                || code.len() > ToolCallRecord::MAX_ERROR_CODE_LEN
                || code.contains('\0')
        })
        || error_detail.as_deref().is_some_and(|detail| {
            detail.is_empty()
                || detail.len() > ToolCallRecord::MAX_ERROR_DETAIL_LEN
                || detail.contains('\0')
        })
    {
        return Err(AgentError::Store("invalid tool call resolution".into()));
    }
    Ok(())
}

fn resolution_error(resolution: &ToolCallResolution) -> (Option<String>, Option<String>) {
    match resolution {
        ToolCallResolution::Failed {
            error_code,
            error_detail,
            ..
        } => (Some(error_code.clone()), error_detail.clone()),
        ToolCallResolution::Completed { .. } | ToolCallResolution::Cancelled { .. } => (None, None),
    }
}

fn immutable_request_matches(
    model: &entities::tool_call::Model,
    call: &ToolCallRecord,
    created_at: DateTime<Utc>,
) -> bool {
    model.chat_id == call.chat_id.0
        && model.turn_id == call.turn_id.0
        && model.provider_id == call.provider_id
        && model.name == call.name
        && model.arguments == call.arguments
        && model.execution == call.execution.as_str()
        && model.created_at == created_at
}

fn terminal_authority_matches(
    model: &entities::tool_call::Model,
    authority: ResolutionAuthority,
) -> bool {
    match authority {
        ResolutionAuthority::Server => {
            model.execution == ToolCallExecution::Server.as_str()
                && model.client_lease_token.is_none()
        }
        ResolutionAuthority::LiveClient { .. } | ResolutionAuthority::ExpiredClient { .. } => {
            model.execution == ToolCallExecution::Client.as_str()
                && Some(ChatId(model.chat_id)) == authority.chat_id()
                && model.client_lease_token == authority.lease_token()
        }
    }
}

fn terminal_payload_matches(
    model: &entities::tool_call::Model,
    resolution: &ToolCallResolution,
) -> bool {
    let (error_code, error_detail) = resolution_error(resolution);
    model.status == resolution.status().as_str()
        && model.result.as_deref() == Some(resolution.result())
        && model.error_code == error_code
        && model.error_detail == error_detail
}

pub(in crate::db) fn tool_call_from_model(
    model: entities::tool_call::Model,
) -> Result<ToolCallRecord> {
    Ok(ToolCallRecord {
        id: CallId(model.id),
        chat_id: ChatId(model.chat_id),
        turn_id: crate::id::TurnId(model.turn_id),
        provider_id: model.provider_id,
        name: model.name,
        arguments: model.arguments,
        execution: execution_from_db(&model.execution)?,
        status: status_from_db(&model.status)?,
        result: model.result,
        error_code: model.error_code,
        error_detail: model.error_detail,
        client_executor_id: model.client_executor_id,
        client_lease_expires_at: model.client_lease_expires_at,
        created_at: model.created_at,
        resolved_at: model.resolved_at,
    })
}

fn client_claim_from_model(model: entities::tool_call::Model) -> Result<ClientToolCallClaim> {
    let lease_token = model.client_lease_token.ok_or_else(|| {
        AgentError::Store("claimed client tool call is missing its lease token".into())
    })?;
    Ok(ClientToolCallClaim {
        call: tool_call_from_model(model)?,
        lease_token,
    })
}

fn execution_from_db(value: &str) -> Result<ToolCallExecution> {
    match value {
        "server" => Ok(ToolCallExecution::Server),
        "client" => Ok(ToolCallExecution::Client),
        _ => Err(AgentError::Store(format!("invalid tool execution {value}"))),
    }
}

fn status_from_db(value: &str) -> Result<ToolCallStatus> {
    match value {
        "pending" => Ok(ToolCallStatus::Pending),
        "completed" => Ok(ToolCallStatus::Completed),
        "failed" => Ok(ToolCallStatus::Failed),
        "cancelled" => Ok(ToolCallStatus::Cancelled),
        _ => Err(AgentError::Store(format!("invalid tool status {value}"))),
    }
}
