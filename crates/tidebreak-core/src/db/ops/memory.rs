use std::collections::HashSet;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseTransaction, EntityTrait,
    QueryFilter, QueryOrder, Set, TransactionTrait,
};

use crate::memory::{
    MemoryBackend, MemoryCapLevel, MemoryCapability, MemoryCaps, MemoryDigest, MemoryError,
    MemoryEvidence, MemoryIngestReceipt, MemoryIngestRequest, MemoryKind, MemoryLinkRelation,
    MemoryListFilter, MemoryRecord, MemoryRecordId, MemoryRecordUpdate, MemoryResult,
    MemoryRevision, MemoryRevisionId, MemoryScope, MemorySearchHit, MemorySearchRequest,
    MemoryStatus, MemoryStatusChange, MemoryWriteReceipt, MemoryWriteState,
    MAX_MEMORY_SEARCH_RESULTS,
};
use crate::{OwnerId, RepoId};

use super::super::{entities, DbStore};

#[derive(Debug, Clone, Copy)]
struct ScopeState {
    active_record_cap: usize,
    digest_byte_cap: usize,
}

fn backend_err(error: impl std::fmt::Display) -> MemoryError {
    MemoryError::Backend(error.to_string())
}

fn scope_ref(scope: MemoryScope) -> String {
    scope
        .repo_id()
        .map_or_else(String::new, |repo_id| repo_id.to_string())
}

fn scope_condition(scope: MemoryScope) -> Condition {
    let condition =
        Condition::all().add(entities::memory_record::Column::ScopeKind.eq(scope.kind_str()));
    match scope {
        MemoryScope::Personal => condition.add(entities::memory_record::Column::RepoId.is_null()),
        MemoryScope::Repo { repo_id } => {
            condition.add(entities::memory_record::Column::RepoId.eq(repo_id.0))
        }
    }
}

async fn lock_scope(
    transaction: &DatabaseTransaction,
    owner: &OwnerId,
    scope: MemoryScope,
) -> MemoryResult<ScopeState> {
    if let MemoryScope::Repo { repo_id } = scope {
        let exists = entities::code_repo::Entity::find_by_id(repo_id.0)
            .filter(entities::code_repo::Column::Owner.eq(owner.as_str()))
            .one(transaction)
            .await
            .map_err(backend_err)?
            .is_some();
        if !exists {
            return Err(MemoryError::ScopeNotFound);
        }
    }

    let now = Utc::now();
    let scope_ref = scope_ref(scope);
    entities::memory_scope_state::Entity::insert(entities::memory_scope_state::ActiveModel {
        owner: Set(owner.as_str().to_owned()),
        scope_kind: Set(scope.kind_str().to_owned()),
        scope_ref: Set(scope_ref.clone()),
        auto_commit: Set(false),
        active_record_cap: Set(crate::DEFAULT_MEMORY_ACTIVE_RECORD_CAP as i64),
        digest_byte_cap: Set(crate::DEFAULT_MEMORY_DIGEST_BYTES as i64),
        created_at: Set(now),
        updated_at: Set(now),
    })
    .on_conflict(
        OnConflict::columns([
            entities::memory_scope_state::Column::Owner,
            entities::memory_scope_state::Column::ScopeKind,
            entities::memory_scope_state::Column::ScopeRef,
        ])
        .do_nothing()
        .to_owned(),
    )
    .exec_without_returning(transaction)
    .await
    .map_err(backend_err)?;

    let locked = entities::memory_scope_state::Entity::update_many()
        .col_expr(
            entities::memory_scope_state::Column::UpdatedAt,
            Expr::col(entities::memory_scope_state::Column::UpdatedAt),
        )
        .filter(entities::memory_scope_state::Column::Owner.eq(owner.as_str()))
        .filter(entities::memory_scope_state::Column::ScopeKind.eq(scope.kind_str()))
        .filter(entities::memory_scope_state::Column::ScopeRef.eq(&scope_ref))
        .exec(transaction)
        .await
        .map_err(backend_err)?;
    if locked.rows_affected != 1 {
        return Err(MemoryError::Backend(
            "memory scope lock row did not persist".to_owned(),
        ));
    }

    let state = entities::memory_scope_state::Entity::find()
        .filter(entities::memory_scope_state::Column::Owner.eq(owner.as_str()))
        .filter(entities::memory_scope_state::Column::ScopeKind.eq(scope.kind_str()))
        .filter(entities::memory_scope_state::Column::ScopeRef.eq(scope_ref))
        .one(transaction)
        .await
        .map_err(backend_err)?
        .ok_or_else(|| MemoryError::Backend("memory scope lock row disappeared".to_owned()))?;
    Ok(ScopeState {
        active_record_cap: usize::try_from(state.active_record_cap)
            .map_err(|_| MemoryError::Backend("memory active-record cap is invalid".to_owned()))?,
        digest_byte_cap: usize::try_from(state.digest_byte_cap)
            .map_err(|_| MemoryError::Backend("memory digest cap is invalid".to_owned()))?,
    })
}

async fn read_scope_state<C>(
    conn: &C,
    owner: &OwnerId,
    scope: MemoryScope,
) -> MemoryResult<ScopeState>
where
    C: ConnectionTrait,
{
    if let Some(state) = entities::memory_scope_state::Entity::find()
        .filter(entities::memory_scope_state::Column::Owner.eq(owner.as_str()))
        .filter(entities::memory_scope_state::Column::ScopeKind.eq(scope.kind_str()))
        .filter(entities::memory_scope_state::Column::ScopeRef.eq(scope_ref(scope)))
        .one(conn)
        .await
        .map_err(backend_err)?
    {
        return Ok(ScopeState {
            active_record_cap: usize::try_from(state.active_record_cap).map_err(|_| {
                MemoryError::Backend("memory active-record cap is invalid".to_owned())
            })?,
            digest_byte_cap: usize::try_from(state.digest_byte_cap)
                .map_err(|_| MemoryError::Backend("memory digest cap is invalid".to_owned()))?,
        });
    }
    if let MemoryScope::Repo { repo_id } = scope {
        let exists = entities::code_repo::Entity::find_by_id(repo_id.0)
            .filter(entities::code_repo::Column::Owner.eq(owner.as_str()))
            .one(conn)
            .await
            .map_err(backend_err)?
            .is_some();
        if !exists {
            return Err(MemoryError::ScopeNotFound);
        }
    }
    Ok(ScopeState {
        active_record_cap: crate::DEFAULT_MEMORY_ACTIVE_RECORD_CAP,
        digest_byte_cap: crate::DEFAULT_MEMORY_DIGEST_BYTES,
    })
}

fn parse_scope(kind: &str, repo_id: Option<uuid::Uuid>) -> MemoryResult<MemoryScope> {
    match (kind, repo_id) {
        ("personal", None) => Ok(MemoryScope::Personal),
        ("repo", Some(repo_id)) => Ok(MemoryScope::Repo {
            repo_id: RepoId(repo_id),
        }),
        _ => Err(MemoryError::Backend(format!(
            "invalid memory scope {kind:?} with repo {repo_id:?}"
        ))),
    }
}

fn parse_kind(kind: &str) -> MemoryResult<MemoryKind> {
    match kind {
        "fact" => Ok(MemoryKind::Fact),
        "preference" => Ok(MemoryKind::Preference),
        "lesson" => Ok(MemoryKind::Lesson),
        "reference" => Ok(MemoryKind::Reference),
        other => Err(MemoryError::Backend(format!(
            "invalid memory kind {other:?}"
        ))),
    }
}

fn parse_status(status: &str) -> MemoryResult<MemoryStatus> {
    match status {
        "tracking" => Ok(MemoryStatus::Tracking),
        "proposed" => Ok(MemoryStatus::Proposed),
        "active" => Ok(MemoryStatus::Active),
        "archived" => Ok(MemoryStatus::Archived),
        "rejected" => Ok(MemoryStatus::Rejected),
        other => Err(MemoryError::Backend(format!(
            "invalid memory status {other:?}"
        ))),
    }
}

fn record_from_model(model: entities::memory_record::Model) -> MemoryResult<MemoryRecord> {
    let observation_count = u32::try_from(model.observation_count)
        .map_err(|_| MemoryError::Backend("memory observation count is invalid".to_owned()))?;
    let record = MemoryRecord {
        id: MemoryRecordId(model.id),
        scope: parse_scope(&model.scope_kind, model.repo_id)?,
        kind: parse_kind(&model.kind)?,
        status: parse_status(&model.status)?,
        title: model.title,
        body: model.body,
        provenance: serde_json::from_value(model.provenance).map_err(backend_err)?,
        links: serde_json::from_value(model.links).map_err(backend_err)?,
        expires_at: model.expires_at,
        superseded_by: model.superseded_by.map(MemoryRecordId),
        observation_count,
        revision: model.revision,
        created_at: model.created_at,
        updated_at: model.updated_at,
    };
    record
        .validate()
        .map_err(|error| MemoryError::Backend(error.to_string()))?;
    Ok(record)
}

fn active_model(
    owner: &OwnerId,
    record: &MemoryRecord,
) -> MemoryResult<entities::memory_record::ActiveModel> {
    Ok(entities::memory_record::ActiveModel {
        id: Set(record.id.0),
        owner: Set(owner.as_str().to_owned()),
        scope_kind: Set(record.scope.kind_str().to_owned()),
        repo_id: Set(record.scope.repo_id().map(|repo_id| repo_id.0)),
        kind: Set(record.kind.as_str().to_owned()),
        status: Set(record.status.as_str().to_owned()),
        title: Set(record.title.clone()),
        body: Set(record.body.clone()),
        provenance: Set(serde_json::to_value(&record.provenance).map_err(backend_err)?),
        links: Set(serde_json::to_value(&record.links).map_err(backend_err)?),
        expires_at: Set(record.expires_at),
        superseded_by: Set(record.superseded_by.map(|id| id.0)),
        observation_count: Set(i64::from(record.observation_count)),
        revision: Set(record.revision),
        created_at: Set(record.created_at),
        updated_at: Set(record.updated_at),
    })
}

async fn update_record_on(
    transaction: &DatabaseTransaction,
    owner: &OwnerId,
    record: &MemoryRecord,
    expected_revision: i64,
) -> MemoryResult<()> {
    let updated = entities::memory_record::Entity::update_many()
        .set(entities::memory_record::ActiveModel {
            kind: Set(record.kind.as_str().to_owned()),
            status: Set(record.status.as_str().to_owned()),
            title: Set(record.title.clone()),
            body: Set(record.body.clone()),
            provenance: Set(serde_json::to_value(&record.provenance).map_err(backend_err)?),
            links: Set(serde_json::to_value(&record.links).map_err(backend_err)?),
            expires_at: Set(record.expires_at),
            superseded_by: Set(record.superseded_by.map(|id| id.0)),
            observation_count: Set(i64::from(record.observation_count)),
            revision: Set(record.revision),
            updated_at: Set(record.updated_at),
            ..Default::default()
        })
        .filter(entities::memory_record::Column::Id.eq(record.id.0))
        .filter(entities::memory_record::Column::Owner.eq(owner.as_str()))
        .filter(entities::memory_record::Column::Revision.eq(expected_revision))
        .exec(transaction)
        .await
        .map_err(backend_err)?;
    if updated.rows_affected == 1 {
        Ok(())
    } else {
        let current = entities::memory_record::Entity::find_by_id(record.id.0)
            .filter(entities::memory_record::Column::Owner.eq(owner.as_str()))
            .one(transaction)
            .await
            .map_err(backend_err)?;
        match current {
            Some(current) => Err(MemoryError::RevisionConflict {
                current_revision: current.revision,
            }),
            None => Err(MemoryError::NotFound),
        }
    }
}

async fn append_revision(
    transaction: &DatabaseTransaction,
    owner: &OwnerId,
    record: &MemoryRecord,
) -> MemoryResult<()> {
    entities::memory_revision::ActiveModel {
        id: Set(MemoryRevisionId::new().0),
        record_id: Set(record.id.0),
        owner: Set(owner.as_str().to_owned()),
        ordinal: Set(record.revision),
        snapshot: Set(serde_json::to_value(record).map_err(backend_err)?),
        created_at: Set(record.updated_at),
    }
    .insert(transaction)
    .await
    .map_err(backend_err)?;
    Ok(())
}

async fn find_record_on<C>(
    conn: &C,
    owner: &OwnerId,
    id: MemoryRecordId,
) -> MemoryResult<Option<MemoryRecord>>
where
    C: ConnectionTrait,
{
    entities::memory_record::Entity::find_by_id(id.0)
        .filter(entities::memory_record::Column::Owner.eq(owner.as_str()))
        .one(conn)
        .await
        .map_err(backend_err)?
        .map(record_from_model)
        .transpose()
}

async fn validate_evidence<C>(conn: &C, owner: &OwnerId, record: &MemoryRecord) -> MemoryResult<()>
where
    C: ConnectionTrait,
{
    for evidence in &record.provenance.evidence {
        let resolves = match evidence {
            MemoryEvidence::Message { message_id } => {
                let Some(message) = entities::message::Entity::find_by_id(message_id.0)
                    .one(conn)
                    .await
                    .map_err(backend_err)?
                else {
                    return Err(MemoryError::EvidenceNotFound(format!(
                        "message {message_id}"
                    )));
                };
                // A chat is a session (decision 48): the message's chat id
                // names a `session` row, whose owner column gates it.
                entities::session::Entity::find_by_id(message.chat_id)
                    .filter(entities::session::Column::Owner.eq(owner.as_str()))
                    .one(conn)
                    .await
                    .map_err(backend_err)?
                    .is_some()
            }
            MemoryEvidence::Event { session_id, seq } => {
                entities::event::Entity::find_by_id((session_id.0, *seq))
                    .filter(entities::event::Column::Owner.eq(owner.as_str()))
                    .one(conn)
                    .await
                    .map_err(backend_err)?
                    .is_some()
            }
        };
        if !resolves {
            return Err(MemoryError::EvidenceNotFound(match evidence {
                MemoryEvidence::Message { message_id } => format!("message {message_id}"),
                MemoryEvidence::Event { session_id, seq } => format!("event {session_id}:{seq}"),
            }));
        }
    }
    Ok(())
}

async fn validate_links<C>(conn: &C, owner: &OwnerId, record: &MemoryRecord) -> MemoryResult<()>
where
    C: ConnectionTrait,
{
    for link in &record.links {
        let Some(target) = find_record_on(conn, owner, link.record_id).await? else {
            return Err(MemoryError::InvalidRecord(format!(
                "linked memory record {} does not exist",
                link.record_id
            )));
        };
        if matches!(
            link.relation,
            MemoryLinkRelation::Updates | MemoryLinkRelation::Supersedes
        ) && target.scope != record.scope
        {
            return Err(MemoryError::InvalidRecord(
                "an update or superseding link must stay in one memory scope".to_owned(),
            ));
        }
    }
    Ok(())
}

async fn validate_references<C>(
    conn: &C,
    owner: &OwnerId,
    record: &MemoryRecord,
) -> MemoryResult<()>
where
    C: ConnectionTrait,
{
    record.validate()?;
    validate_evidence(conn, owner, record).await?;
    validate_links(conn, owner, record).await
}

async fn active_records_on<C>(
    conn: &C,
    owner: &OwnerId,
    scope: MemoryScope,
) -> MemoryResult<Vec<MemoryRecord>>
where
    C: ConnectionTrait,
{
    entities::memory_record::Entity::find()
        .filter(entities::memory_record::Column::Owner.eq(owner.as_str()))
        .filter(scope_condition(scope))
        .filter(entities::memory_record::Column::Status.eq(MemoryStatus::Active.as_str()))
        .order_by_desc(entities::memory_record::Column::UpdatedAt)
        .order_by_asc(entities::memory_record::Column::Id)
        .all(conn)
        .await
        .map_err(backend_err)?
        .into_iter()
        .map(record_from_model)
        .collect()
}

/// Deterministic digest renderer. An unchanged store re-renders byte-identical markdown.
pub(crate) fn render_digest(
    scope: MemoryScope,
    mut records: Vec<MemoryRecord>,
    byte_cap: usize,
) -> MemoryResult<MemoryDigest> {
    records.sort_by(|left, right| {
        kind_order(left.kind)
            .cmp(&kind_order(right.kind))
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.id.0.cmp(&right.id.0))
    });

    let mut markdown = String::new();
    if !records.is_empty() {
        markdown.push_str("## Tidebreak memory\n\n");
        markdown.push_str(
            "These are dated point-in-time claims from Tidebreak. Treat this conversation as newer evidence.\n",
        );
        let mut previous_kind = None;
        for record in &records {
            if previous_kind != Some(record.kind) {
                markdown.push_str("\n### ");
                markdown.push_str(record.kind.heading());
                markdown.push('\n');
                previous_kind = Some(record.kind);
            }
            markdown.push_str("- ");
            markdown.push_str(&record.updated_at.format("%Y-%m-%d").to_string());
            markdown.push_str(" — ");
            markdown.push_str(&record.title);
            markdown.push('\n');
        }
    }
    if markdown.len() > byte_cap {
        return Err(MemoryError::DigestCapExceeded { cap: byte_cap });
    }
    Ok(MemoryDigest {
        scope,
        byte_len: markdown.len(),
        byte_cap,
        record_count: records.len(),
        markdown,
    })
}

const fn kind_order(kind: MemoryKind) -> u8 {
    match kind {
        MemoryKind::Fact => 0,
        MemoryKind::Preference => 1,
        MemoryKind::Lesson => 2,
        MemoryKind::Reference => 3,
    }
}

async fn validate_scope_caps(
    transaction: &DatabaseTransaction,
    owner: &OwnerId,
    scope: MemoryScope,
    state: ScopeState,
) -> MemoryResult<()> {
    let records = active_records_on(transaction, owner, scope).await?;
    if records.len() > state.active_record_cap {
        return Err(MemoryError::ActiveRecordCapExceeded {
            cap: state.active_record_cap,
        });
    }
    render_digest(scope, records, state.digest_byte_cap)?;
    Ok(())
}

fn write_receipt(record: MemoryRecord) -> MemoryWriteReceipt {
    MemoryWriteReceipt {
        state: MemoryWriteState::Committed,
        record,
    }
}

#[async_trait]
impl MemoryBackend for DbStore {
    fn caps(&self) -> MemoryCaps {
        MemoryCaps {
            extraction: MemoryCapLevel::Unsupported,
            lexical_search: MemoryCapLevel::Supported,
            semantic_search: MemoryCapLevel::Unsupported,
            consolidation: MemoryCapLevel::Unsupported,
            context_assembly: MemoryCapLevel::Supported,
            revision_history: MemoryCapLevel::Supported,
            verified_delete: MemoryCapLevel::Supported,
            asynchronous_writes: MemoryCapLevel::Unsupported,
            agent_editable_surfaces: MemoryCapLevel::Supported,
        }
    }

    async fn put(&self, owner: &OwnerId, record: MemoryRecord) -> MemoryResult<MemoryWriteReceipt> {
        record.validate()?;
        if record.revision != 1 {
            return Err(MemoryError::InvalidRecord(
                "a new memory record must start at revision 1".to_owned(),
            ));
        }
        let transaction = self.conn.begin().await.map_err(backend_err)?;
        let state = lock_scope(&transaction, owner, record.scope).await?;
        if entities::memory_record::Entity::find_by_id(record.id.0)
            .one(&transaction)
            .await
            .map_err(backend_err)?
            .is_some()
        {
            return Err(MemoryError::AlreadyExists);
        }
        validate_references(&transaction, owner, &record).await?;
        active_model(owner, &record)?
            .insert(&transaction)
            .await
            .map_err(backend_err)?;
        append_revision(&transaction, owner, &record).await?;
        if record.status.is_authoritative() {
            validate_scope_caps(&transaction, owner, record.scope, state).await?;
        }
        transaction.commit().await.map_err(backend_err)?;
        Ok(write_receipt(record))
    }

    async fn ingest(
        &self,
        _owner: &OwnerId,
        _request: MemoryIngestRequest,
    ) -> MemoryResult<MemoryIngestReceipt> {
        Err(MemoryError::Unsupported(MemoryCapability::Extraction))
    }

    async fn get(&self, owner: &OwnerId, id: MemoryRecordId) -> MemoryResult<Option<MemoryRecord>> {
        find_record_on(&self.conn, owner, id).await
    }

    async fn list(
        &self,
        owner: &OwnerId,
        filter: MemoryListFilter,
    ) -> MemoryResult<Vec<MemoryRecord>> {
        let mut query = entities::memory_record::Entity::find()
            .filter(entities::memory_record::Column::Owner.eq(owner.as_str()));
        if let Some(scope) = filter.scope {
            query = query.filter(scope_condition(scope));
        }
        if !filter.statuses.is_empty() {
            query = query.filter(
                entities::memory_record::Column::Status
                    .is_in(filter.statuses.into_iter().map(MemoryStatus::as_str)),
            );
        }
        if !filter.kinds.is_empty() {
            query = query.filter(
                entities::memory_record::Column::Kind
                    .is_in(filter.kinds.into_iter().map(MemoryKind::as_str)),
            );
        }
        query
            .order_by_desc(entities::memory_record::Column::UpdatedAt)
            .order_by_asc(entities::memory_record::Column::Id)
            .all(&self.conn)
            .await
            .map_err(backend_err)?
            .into_iter()
            .map(record_from_model)
            .collect()
    }

    async fn update(
        &self,
        owner: &OwnerId,
        update: MemoryRecordUpdate,
    ) -> MemoryResult<MemoryWriteReceipt> {
        let transaction = self.conn.begin().await.map_err(backend_err)?;
        let Some(existing) = find_record_on(&transaction, owner, update.id).await? else {
            return Err(MemoryError::NotFound);
        };
        let state = lock_scope(&transaction, owner, existing.scope).await?;
        let Some(existing) = find_record_on(&transaction, owner, update.id).await? else {
            return Err(MemoryError::NotFound);
        };
        if existing.revision != update.expected_revision {
            return Err(MemoryError::RevisionConflict {
                current_revision: existing.revision,
            });
        }
        let revision = existing.revision.checked_add(1).ok_or_else(|| {
            MemoryError::Backend("memory revision counter is exhausted".to_owned())
        })?;
        let record = MemoryRecord {
            kind: update.kind,
            title: update.title,
            body: update.body,
            provenance: update.provenance,
            links: update.links,
            expires_at: update.expires_at,
            observation_count: update.observation_count,
            revision,
            updated_at: Utc::now(),
            ..existing
        };
        validate_references(&transaction, owner, &record).await?;
        update_record_on(&transaction, owner, &record, update.expected_revision).await?;
        append_revision(&transaction, owner, &record).await?;
        if record.status.is_authoritative() {
            validate_scope_caps(&transaction, owner, record.scope, state).await?;
        }
        transaction.commit().await.map_err(backend_err)?;
        Ok(write_receipt(record))
    }

    async fn set_status(
        &self,
        owner: &OwnerId,
        change: MemoryStatusChange,
    ) -> MemoryResult<MemoryWriteReceipt> {
        let transaction = self.conn.begin().await.map_err(backend_err)?;
        let Some(existing) = find_record_on(&transaction, owner, change.id).await? else {
            return Err(MemoryError::NotFound);
        };
        let state = lock_scope(&transaction, owner, existing.scope).await?;
        let Some(existing) = find_record_on(&transaction, owner, change.id).await? else {
            return Err(MemoryError::NotFound);
        };
        if existing.revision != change.expected_revision {
            return Err(MemoryError::RevisionConflict {
                current_revision: existing.revision,
            });
        }
        if existing.status == change.status {
            transaction.commit().await.map_err(backend_err)?;
            return Ok(write_receipt(existing));
        }
        if !existing.status.can_transition_to(change.status) {
            return Err(MemoryError::InvalidStatusTransition {
                from: existing.status,
                to: change.status,
            });
        }

        let now = Utc::now();
        if change.status == MemoryStatus::Active {
            let sources = existing
                .links
                .iter()
                .filter(|link| {
                    matches!(
                        link.relation,
                        MemoryLinkRelation::Updates | MemoryLinkRelation::Supersedes
                    )
                })
                .map(|link| link.record_id)
                .collect::<HashSet<_>>();
            for source_id in sources {
                let Some(source) = find_record_on(&transaction, owner, source_id).await? else {
                    return Err(MemoryError::InvalidRecord(format!(
                        "source memory record {source_id} does not exist"
                    )));
                };
                if source.scope != existing.scope || source.status != MemoryStatus::Active {
                    return Err(MemoryError::InvalidRecord(format!(
                        "source memory record {source_id} is not active in this scope"
                    )));
                }
                let source_revision = source.revision.checked_add(1).ok_or_else(|| {
                    MemoryError::Backend("memory revision counter is exhausted".to_owned())
                })?;
                let archived = MemoryRecord {
                    status: MemoryStatus::Archived,
                    superseded_by: Some(existing.id),
                    revision: source_revision,
                    updated_at: now,
                    ..source
                };
                update_record_on(&transaction, owner, &archived, source_revision - 1).await?;
                append_revision(&transaction, owner, &archived).await?;
            }
        }

        let revision = existing.revision.checked_add(1).ok_or_else(|| {
            MemoryError::Backend("memory revision counter is exhausted".to_owned())
        })?;
        let record = MemoryRecord {
            status: change.status,
            revision,
            updated_at: now,
            ..existing
        };
        update_record_on(&transaction, owner, &record, change.expected_revision).await?;
        append_revision(&transaction, owner, &record).await?;
        if record.status.is_authoritative() {
            validate_scope_caps(&transaction, owner, record.scope, state).await?;
        }
        transaction.commit().await.map_err(backend_err)?;
        Ok(write_receipt(record))
    }

    async fn delete(&self, owner: &OwnerId, id: MemoryRecordId) -> MemoryResult<bool> {
        let transaction = self.conn.begin().await.map_err(backend_err)?;
        let Some(existing) = find_record_on(&transaction, owner, id).await? else {
            transaction.commit().await.map_err(backend_err)?;
            return Ok(false);
        };
        lock_scope(&transaction, owner, existing.scope).await?;
        let deleted = entities::memory_record::Entity::delete_many()
            .filter(entities::memory_record::Column::Id.eq(id.0))
            .filter(entities::memory_record::Column::Owner.eq(owner.as_str()))
            .exec(&transaction)
            .await
            .map_err(backend_err)?;
        if deleted.rows_affected != 1 {
            return Err(MemoryError::NotFound);
        }
        let record_remains = entities::memory_record::Entity::find_by_id(id.0)
            .one(&transaction)
            .await
            .map_err(backend_err)?
            .is_some();
        let revision_remains = entities::memory_revision::Entity::find()
            .filter(entities::memory_revision::Column::RecordId.eq(id.0))
            .one(&transaction)
            .await
            .map_err(backend_err)?
            .is_some();
        if record_remains || revision_remains {
            return Err(MemoryError::Backend(
                "memory delete did not remove the record and revisions".to_owned(),
            ));
        }
        transaction.commit().await.map_err(backend_err)?;
        Ok(true)
    }

    async fn search(
        &self,
        owner: &OwnerId,
        request: MemorySearchRequest,
    ) -> MemoryResult<Vec<MemorySearchHit>> {
        let query = request.query.trim().to_lowercase();
        if query.is_empty() {
            return Err(MemoryError::InvalidRecord(
                "memory search query must not be empty".to_owned(),
            ));
        }
        if request.limit == 0 || request.limit > MAX_MEMORY_SEARCH_RESULTS {
            return Err(MemoryError::InvalidRecord(format!(
                "memory search limit must be between 1 and {MAX_MEMORY_SEARCH_RESULTS}"
            )));
        }
        let records = self
            .list(
                owner,
                MemoryListFilter {
                    scope: request.scope,
                    statuses: request.statuses,
                    kinds: Vec::new(),
                },
            )
            .await?;
        let tokens = query.split_whitespace().collect::<Vec<_>>();
        let mut hits = records
            .into_iter()
            .filter_map(|record| lexical_hit(&record, &query, &tokens))
            .collect::<Vec<_>>();
        hits.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| right.updated_at.cmp(&left.updated_at))
                .then_with(|| left.record_id.0.cmp(&right.record_id.0))
        });
        hits.truncate(request.limit);
        Ok(hits)
    }

    async fn assemble_context(
        &self,
        owner: &OwnerId,
        scope: MemoryScope,
    ) -> MemoryResult<MemoryDigest> {
        let state = read_scope_state(&self.conn, owner, scope).await?;
        let records = active_records_on(&self.conn, owner, scope).await?;
        render_digest(scope, records, state.digest_byte_cap)
    }

    async fn revision_history(
        &self,
        owner: &OwnerId,
        id: MemoryRecordId,
    ) -> MemoryResult<Vec<MemoryRevision>> {
        entities::memory_revision::Entity::find()
            .filter(entities::memory_revision::Column::Owner.eq(owner.as_str()))
            .filter(entities::memory_revision::Column::RecordId.eq(id.0))
            .order_by_asc(entities::memory_revision::Column::Ordinal)
            .all(&self.conn)
            .await
            .map_err(backend_err)?
            .into_iter()
            .map(|model| {
                let snapshot = serde_json::from_value(model.snapshot).map_err(backend_err)?;
                Ok(MemoryRevision {
                    id: MemoryRevisionId(model.id),
                    record_id: MemoryRecordId(model.record_id),
                    ordinal: model.ordinal,
                    snapshot,
                    created_at: model.created_at,
                })
            })
            .collect()
    }
}

fn lexical_hit(record: &MemoryRecord, query: &str, tokens: &[&str]) -> Option<MemorySearchHit> {
    let title = record.title.to_lowercase();
    let body = record.body.to_lowercase();
    if !tokens
        .iter()
        .all(|token| title.contains(token) || body.contains(token))
    {
        return None;
    }

    let phrase_title = title.match_indices(query).count() as u32;
    let phrase_body = body.match_indices(query).count() as u32;
    let token_title = tokens
        .iter()
        .map(|token| title.match_indices(token).count() as u32)
        .sum::<u32>();
    let token_body = tokens
        .iter()
        .map(|token| body.match_indices(token).count() as u32)
        .sum::<u32>();
    let score = phrase_title
        .saturating_mul(12)
        .saturating_add(phrase_body.saturating_mul(6))
        .saturating_add(token_title.saturating_mul(3))
        .saturating_add(token_body);
    let matching_line = record
        .body
        .lines()
        .find(|line| {
            let normalized = line.to_lowercase();
            normalized.contains(query) || tokens.iter().any(|token| normalized.contains(token))
        })
        .or_else(|| record.body.lines().find(|line| !line.trim().is_empty()))
        .unwrap_or_default()
        .to_owned();
    Some(MemorySearchHit {
        record_id: record.id,
        title: record.title.clone(),
        updated_at: record.updated_at,
        matching_line,
        score,
    })
}
