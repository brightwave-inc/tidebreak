use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder, Set, TransactionTrait,
};

use crate::attention::{Attention, AttentionSource, AttentionState, FenceReason};
use crate::code::{
    CodeSession, CodeSessionId, CodeSessionKind, CodeSessionLifecycle, CodeSubagentSummary,
    HarnessKind, WorkspaceId,
};
use crate::error::{AgentError, Result};
use crate::OwnerId;
use crate::PermissionMode;
use crate::ReasoningEffort;

use super::super::super::{entities, store_err, DbStore};
use super::acquire_code_session_write_lock;

/// One durable permission-mode transition owned by an exact worker epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionModeChangeIntent {
    pub session_id: CodeSessionId,
    pub owner: OwnerId,
    pub revision: i64,
    pub previous_mode: PermissionMode,
    pub requested_mode: PermissionMode,
    pub lifecycle: CodeSessionLifecycle,
    pub worker_epoch: i64,
}

/// A session plus the unresolved mode transition that blocks recovery.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingPermissionModeChange {
    pub session: CodeSession,
    pub intent: PermissionModeChangeIntent,
}

/// The model-dependent execution settings one session actively uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeSessionExecutionSettings {
    /// Engine model id, when the session selected one.
    pub model: Option<String>,
    /// Reasoning effort, or the engine default when absent.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Whether turns use the selected model's fast service tier.
    pub fast_mode: bool,
}

impl From<&CodeSession> for CodeSessionExecutionSettings {
    fn from(session: &CodeSession) -> Self {
        Self {
            model: session.model.clone(),
            reasoning_effort: session.reasoning_effort,
            fast_mode: session.fast_mode,
        }
    }
}

/// Insert a session row. The row belongs to `session.owner`, denormalized
/// from the workspace it runs in.
pub async fn insert_session(store: &DbStore, session: &CodeSession) -> Result<()> {
    entities::code_session::ActiveModel {
        id: Set(session.id.0),
        owner: Set(session.owner.as_str().to_owned()),
        workspace_id: Set(session.workspace_id.0),
        kind: Set(session.kind.as_str().to_owned()),
        harness_kind: Set(session.harness_kind.as_str().to_owned()),
        harness_version: Set(session.harness_version.clone()),
        harness_resume_ref: Set(session.harness_resume_ref.clone()),
        permission_mode: Set(session.permission_mode.as_str().to_owned()),
        permission_mode_revision: Set(0),
        permission_mode_intent: Set(None),
        permission_mode_intent_revision: Set(None),
        permission_mode_intent_epoch: Set(None),
        permission_mode_intent_lifecycle: Set(None),
        model: Set(session.model.clone()),
        reasoning_effort: Set(session
            .reasoning_effort
            .map(|effort| effort.as_str().to_owned())),
        fast_mode: Set(session.fast_mode),
        lifecycle: Set(session.lifecycle.as_str().to_owned()),
        fence_reason: Set(match &session.fence_reason {
            Some(reason) => Some(serde_json::to_value(reason)?),
            None => None,
        }),
        child_pid: Set(session.child_pid),
        child_process_identity: Set(session.child_process_identity.clone()),
        spawn_epoch: Set(session.spawn_epoch),
        attention_state: Set(serde_json::to_value(&session.attention.state)?),
        attention_source: Set(session.attention.source.as_str().to_owned()),
        unrecognized_event_count: Set(session.unrecognized_event_count),
        subagents: Set(if session.subagents.is_empty() {
            None
        } else {
            Some(serde_json::to_value(&session.subagents)?)
        }),
        created_at: Set(session.created_at),
    }
    .insert(&store.conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

/// Load one of the owner's sessions by id.
///
/// Another owner's session is indistinguishable from a missing one.
pub async fn get_session(
    store: &DbStore,
    owner: &OwnerId,
    id: CodeSessionId,
) -> Result<Option<CodeSession>> {
    let Some(row) = entities::code_session::Entity::find_by_id(id.0)
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    Ok(Some(session_from_row(row)?))
}

/// Load a session by id, whatever owner it belongs to.
///
/// A system path, not a request path: a session worker re-reads the row it
/// was spawned against after bumping the spawn epoch, and the id it holds
/// was authorized when the worker was created. Nothing reachable from a
/// route may call it.
pub async fn get_session_all_owners(
    store: &DbStore,
    id: CodeSessionId,
) -> Result<Option<CodeSession>> {
    let Some(row) = entities::code_session::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    Ok(Some(session_from_row(row)?))
}

/// Replace active execution settings only while the caller still owns the
/// exact session lifecycle, worker epoch, and preceding settings.
///
/// When a worker is live, the caller holds its local session copy until this
/// transaction commits. `None` means a concurrent lifecycle or settings
/// change won, so the caller must release the worker without applying `next`.
pub async fn replace_session_execution_settings(
    store: &DbStore,
    owner: &OwnerId,
    expected: &CodeSession,
    next: &CodeSessionExecutionSettings,
) -> Result<Option<CodeSession>> {
    if &expected.owner != owner {
        return Ok(None);
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, expected.id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(row) = entities::code_session::Entity::find_by_id(expected.id.0)
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    };
    let current = session_from_row(row)?;
    if current.lifecycle != expected.lifecycle
        || current.spawn_epoch != expected.spawn_epoch
        || current.model != expected.model
        || current.reasoning_effort != expected.reasoning_effort
        || current.fast_mode != expected.fast_mode
    {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let updated = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::Model,
            sea_orm::sea_query::Expr::value(next.model.clone()),
        )
        .col_expr(
            entities::code_session::Column::ReasoningEffort,
            sea_orm::sea_query::Expr::value(
                next.reasoning_effort
                    .map(|effort| effort.as_str().to_owned()),
            ),
        )
        .col_expr(
            entities::code_session::Column::FastMode,
            sea_orm::sea_query::Expr::value(next.fast_mode),
        )
        .filter(entities::code_session::Column::Id.eq(expected.id.0))
        .filter(entities::code_session::Column::Owner.eq(expected.owner.as_str()))
        .filter(entities::code_session::Column::Lifecycle.eq(expected.lifecycle.as_str()))
        .filter(entities::code_session::Column::SpawnEpoch.eq(expected.spawn_epoch))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    transaction.commit().await.map_err(store_err)?;
    let mut current = current;
    current.model.clone_from(&next.model);
    current.reasoning_effort = next.reasoning_effort;
    current.fast_mode = next.fast_mode;
    Ok(Some(current))
}

/// Persist a versioned mode-change intent before any live engine mutation.
///
/// The exact owner, lifecycle, worker epoch, confirmed mode, and committed
/// revision form the claim. `None` means that one of them changed or another
/// transition already owns the row, so the caller must not contact the engine.
pub async fn begin_permission_mode_change(
    store: &DbStore,
    owner: &OwnerId,
    expected: &CodeSession,
    requested_mode: PermissionMode,
) -> Result<Option<PermissionModeChangeIntent>> {
    if &expected.owner != owner {
        return Ok(None);
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_permission_mode_change_write_lock(&transaction, owner, expected.id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(row) = entities::code_session::Entity::find_by_id(expected.id.0)
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    };
    let matches = row.lifecycle == expected.lifecycle.as_str()
        && row.spawn_epoch == expected.spawn_epoch
        && row.permission_mode == expected.permission_mode.as_str()
        && row.permission_mode_intent.is_none()
        && row.permission_mode_intent_revision.is_none()
        && row.permission_mode_intent_epoch.is_none()
        && row.permission_mode_intent_lifecycle.is_none();
    if !matches {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let revision = row.permission_mode_revision.checked_add(1).ok_or_else(|| {
        AgentError::Store(format!(
            "code session {} permission-mode revision overflow",
            expected.id
        ))
    })?;
    let intent = PermissionModeChangeIntent {
        session_id: expected.id,
        owner: owner.clone(),
        revision,
        previous_mode: expected.permission_mode,
        requested_mode,
        lifecycle: expected.lifecycle,
        worker_epoch: expected.spawn_epoch,
    };
    let updated = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::PermissionModeIntent,
            sea_orm::sea_query::Expr::value(Some(requested_mode.as_str().to_owned())),
        )
        .col_expr(
            entities::code_session::Column::PermissionModeIntentRevision,
            sea_orm::sea_query::Expr::value(Some(revision)),
        )
        .col_expr(
            entities::code_session::Column::PermissionModeIntentEpoch,
            sea_orm::sea_query::Expr::value(Some(expected.spawn_epoch)),
        )
        .col_expr(
            entities::code_session::Column::PermissionModeIntentLifecycle,
            sea_orm::sea_query::Expr::value(Some(expected.lifecycle.as_str().to_owned())),
        )
        .filter(permission_mode_change_base(owner, &intent))
        .filter(entities::code_session::Column::PermissionModeIntent.is_null())
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(intent))
}

/// Commit one acknowledged native mode change through its exact intent.
pub async fn confirm_permission_mode_change(
    store: &DbStore,
    owner: &OwnerId,
    intent: &PermissionModeChangeIntent,
) -> Result<bool> {
    update_permission_mode_intent(store, owner, intent, PermissionModeIntentUpdate::Confirm).await
}

/// Drop an intent after the engine proves that it did not apply the request.
pub async fn cancel_permission_mode_change(
    store: &DbStore,
    owner: &OwnerId,
    intent: &PermissionModeChangeIntent,
) -> Result<bool> {
    update_permission_mode_intent(store, owner, intent, PermissionModeIntentUpdate::Cancel).await
}

/// Clear an intent whose lifecycle or worker epoch has already moved on.
///
/// The intent identity and reserved revision still have to match. This only
/// drops a transition that can no longer confirm; it never changes the
/// durable permission mode or the newer session state that superseded it.
pub async fn discard_permission_mode_change(
    store: &DbStore,
    owner: &OwnerId,
    intent: &PermissionModeChangeIntent,
) -> Result<bool> {
    if &intent.owner != owner {
        return Ok(false);
    }
    let result = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::PermissionModeIntent,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::code_session::Column::PermissionModeIntentRevision,
            sea_orm::sea_query::Expr::value(Option::<i64>::None),
        )
        .col_expr(
            entities::code_session::Column::PermissionModeIntentEpoch,
            sea_orm::sea_query::Expr::value(Option::<i64>::None),
        )
        .col_expr(
            entities::code_session::Column::PermissionModeIntentLifecycle,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .filter(permission_mode_change_identity(owner, intent))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Fence an exact unresolved intent after its engine worker has stopped.
pub async fn fence_permission_mode_change(
    store: &DbStore,
    owner: &OwnerId,
    intent: &PermissionModeChangeIntent,
    reason: &FenceReason,
) -> Result<Option<CodeSession>> {
    if &intent.owner != owner {
        return Ok(None);
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_permission_mode_change_write_lock(&transaction, owner, intent.session_id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(row) = exact_permission_mode_intent(&transaction, owner, intent).await? else {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    };
    let mut session = session_from_row(row)?;
    let attention = Attention::new(
        AttentionState::Fenced {
            reason: reason.clone(),
        },
        AttentionSource::Lifecycle,
    );
    if crate::attention::should_replace(&session.attention, &attention) {
        session.attention = attention;
    }
    let updated = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::Lifecycle,
            sea_orm::sea_query::Expr::value(CodeSessionLifecycle::Fenced.as_str()),
        )
        .col_expr(
            entities::code_session::Column::FenceReason,
            sea_orm::sea_query::Expr::value(Some(serde_json::to_value(reason)?)),
        )
        .col_expr(
            entities::code_session::Column::ChildPid,
            sea_orm::sea_query::Expr::value(Option::<i64>::None),
        )
        .col_expr(
            entities::code_session::Column::AttentionState,
            sea_orm::sea_query::Expr::value(serde_json::to_value(&session.attention.state)?),
        )
        .col_expr(
            entities::code_session::Column::AttentionSource,
            sea_orm::sea_query::Expr::value(session.attention.source.as_str()),
        )
        .col_expr(
            entities::code_session::Column::PermissionModeIntent,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::code_session::Column::PermissionModeIntentRevision,
            sea_orm::sea_query::Expr::value(Option::<i64>::None),
        )
        .col_expr(
            entities::code_session::Column::PermissionModeIntentEpoch,
            sea_orm::sea_query::Expr::value(Option::<i64>::None),
        )
        .col_expr(
            entities::code_session::Column::PermissionModeIntentLifecycle,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .filter(permission_mode_change_exact(owner, intent))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    transaction.commit().await.map_err(store_err)?;
    get_session(store, owner, intent.session_id).await
}

/// List unresolved intents so startup fences them before attaching workers.
pub async fn list_pending_permission_mode_changes(
    store: &DbStore,
    owner: &OwnerId,
) -> Result<Vec<PendingPermissionModeChange>> {
    entities::code_session::Entity::find()
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_session::Column::PermissionModeIntent.is_not_null())
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|row| {
            let intent = permission_mode_intent_from_row(&row)?;
            let session = session_from_row(row)?;
            Ok(PendingPermissionModeChange { session, intent })
        })
        .collect()
}

enum PermissionModeIntentUpdate {
    Confirm,
    Cancel,
}

async fn acquire_permission_mode_change_write_lock<C>(
    conn: &C,
    owner: &OwnerId,
    session_id: CodeSessionId,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    let locked = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::UnrecognizedEventCount,
            sea_orm::sea_query::Expr::col(entities::code_session::Column::UnrecognizedEventCount),
        )
        .filter(entities::code_session::Column::Id.eq(session_id.0))
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(locked.rows_affected == 1)
}

async fn update_permission_mode_intent(
    store: &DbStore,
    owner: &OwnerId,
    intent: &PermissionModeChangeIntent,
    update: PermissionModeIntentUpdate,
) -> Result<bool> {
    if &intent.owner != owner {
        return Ok(false);
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_permission_mode_change_write_lock(&transaction, owner, intent.session_id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(false);
    }
    if exact_permission_mode_intent(&transaction, owner, intent)
        .await?
        .is_none()
    {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(false);
    }
    let mut query = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::PermissionModeIntent,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::code_session::Column::PermissionModeIntentRevision,
            sea_orm::sea_query::Expr::value(Option::<i64>::None),
        )
        .col_expr(
            entities::code_session::Column::PermissionModeIntentEpoch,
            sea_orm::sea_query::Expr::value(Option::<i64>::None),
        )
        .col_expr(
            entities::code_session::Column::PermissionModeIntentLifecycle,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        );
    if matches!(update, PermissionModeIntentUpdate::Confirm) {
        query = query
            .col_expr(
                entities::code_session::Column::PermissionMode,
                sea_orm::sea_query::Expr::value(intent.requested_mode.as_str()),
            )
            .col_expr(
                entities::code_session::Column::PermissionModeRevision,
                sea_orm::sea_query::Expr::value(intent.revision),
            );
    }
    let updated = query
        .filter(permission_mode_change_exact(owner, intent))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(false);
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(true)
}

fn permission_mode_change_base(owner: &OwnerId, intent: &PermissionModeChangeIntent) -> Condition {
    Condition::all()
        .add(entities::code_session::Column::Id.eq(intent.session_id.0))
        .add(entities::code_session::Column::Owner.eq(owner.as_str()))
        .add(entities::code_session::Column::Lifecycle.eq(intent.lifecycle.as_str()))
        .add(entities::code_session::Column::SpawnEpoch.eq(intent.worker_epoch))
        .add(entities::code_session::Column::PermissionMode.eq(intent.previous_mode.as_str()))
        .add(entities::code_session::Column::PermissionModeRevision.eq(intent.revision - 1))
}

fn permission_mode_change_exact(owner: &OwnerId, intent: &PermissionModeChangeIntent) -> Condition {
    Condition::all()
        .add(permission_mode_change_base(owner, intent))
        .add(permission_mode_change_identity(owner, intent))
}

fn permission_mode_change_identity(
    owner: &OwnerId,
    intent: &PermissionModeChangeIntent,
) -> Condition {
    Condition::all()
        .add(entities::code_session::Column::Id.eq(intent.session_id.0))
        .add(entities::code_session::Column::Owner.eq(owner.as_str()))
        .add(entities::code_session::Column::PermissionModeRevision.eq(intent.revision - 1))
        .add(
            entities::code_session::Column::PermissionModeIntent.eq(intent.requested_mode.as_str()),
        )
        .add(entities::code_session::Column::PermissionModeIntentRevision.eq(intent.revision))
        .add(entities::code_session::Column::PermissionModeIntentEpoch.eq(intent.worker_epoch))
        .add(
            entities::code_session::Column::PermissionModeIntentLifecycle
                .eq(intent.lifecycle.as_str()),
        )
}

async fn exact_permission_mode_intent<C>(
    conn: &C,
    owner: &OwnerId,
    intent: &PermissionModeChangeIntent,
) -> Result<Option<entities::code_session::Model>>
where
    C: ConnectionTrait,
{
    entities::code_session::Entity::find_by_id(intent.session_id.0)
        .filter(permission_mode_change_exact(owner, intent))
        .one(conn)
        .await
        .map_err(store_err)
}

fn permission_mode_intent_from_row(
    row: &entities::code_session::Model,
) -> Result<PermissionModeChangeIntent> {
    let requested = row.permission_mode_intent.as_deref().ok_or_else(|| {
        AgentError::Store(format!(
            "code session {} has an incomplete permission-mode intent",
            row.id
        ))
    })?;
    let requested_mode = PermissionMode::from_str(requested).ok_or_else(|| {
        AgentError::Store(format!(
            "code session {} has unknown permission-mode intent {}",
            row.id, requested
        ))
    })?;
    let previous_mode = PermissionMode::from_str(&row.permission_mode).ok_or_else(|| {
        AgentError::Store(format!(
            "code session {} has unknown permission_mode {}",
            row.id, row.permission_mode
        ))
    })?;
    let lifecycle_token = row
        .permission_mode_intent_lifecycle
        .as_deref()
        .ok_or_else(|| {
            AgentError::Store(format!(
                "code session {} has an incomplete permission-mode intent",
                row.id
            ))
        })?;
    let lifecycle = CodeSessionLifecycle::from_str(lifecycle_token).ok_or_else(|| {
        AgentError::Store(format!(
            "code session {} has unknown permission-mode intent lifecycle {}",
            row.id, lifecycle_token
        ))
    })?;
    let revision = row.permission_mode_intent_revision.ok_or_else(|| {
        AgentError::Store(format!(
            "code session {} has an incomplete permission-mode intent",
            row.id
        ))
    })?;
    if revision != row.permission_mode_revision + 1 {
        return Err(AgentError::Store(format!(
            "code session {} has non-sequential permission-mode intent revision {} after {}",
            row.id, revision, row.permission_mode_revision
        )));
    }
    Ok(PermissionModeChangeIntent {
        session_id: CodeSessionId(row.id),
        owner: OwnerId::new(&row.owner)?,
        revision,
        previous_mode,
        requested_mode,
        lifecycle,
        worker_epoch: row.permission_mode_intent_epoch.ok_or_else(|| {
            AgentError::Store(format!(
                "code session {} has an incomplete permission-mode intent",
                row.id
            ))
        })?,
    })
}

/// Advance the spawn epoch and record the new child pid. Returns the new epoch.
///
/// Serialized on the same session-row lock journal appends take, so a
/// superseded worker cannot keep a live epoch.
pub async fn bump_spawn_epoch(
    store: &DbStore,
    id: CodeSessionId,
    child_pid: Option<i64>,
) -> Result<i64> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, id).await? {
        return Err(AgentError::Store(format!("code session {id} not found")));
    }
    let Some(session) = entities::code_session::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        return Err(AgentError::Store(format!("code session {id} not found")));
    };
    let next = session
        .spawn_epoch
        .checked_add(1)
        .ok_or_else(|| AgentError::Store(format!("code session {id} spawn epoch overflow")))?;
    let updated = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::SpawnEpoch,
            sea_orm::sea_query::Expr::value(next),
        )
        .col_expr(
            entities::code_session::Column::ChildPid,
            sea_orm::sea_query::Expr::value(child_pid),
        )
        .col_expr(
            entities::code_session::Column::ChildProcessIdentity,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .filter(entities::code_session::Column::Id.eq(id.0))
        .filter(entities::code_session::Column::SpawnEpoch.eq(session.spawn_epoch))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "code session {id} spawn epoch changed under the lock"
        )));
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(next)
}

/// The owner's sessions, most recently created first.
pub async fn list_sessions(store: &DbStore, owner: &OwnerId) -> Result<Vec<CodeSession>> {
    entities::code_session::Entity::find()
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .order_by_desc(entities::code_session::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(session_from_row)
        .collect()
}

/// The owner's sessions in one workspace, most recently created first.
pub async fn list_sessions_for_workspace(
    store: &DbStore,
    owner: &OwnerId,
    workspace_id: WorkspaceId,
) -> Result<Vec<CodeSession>> {
    entities::code_session::Entity::find()
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_session::Column::WorkspaceId.eq(workspace_id.0))
        .order_by_desc(entities::code_session::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(session_from_row)
        .collect()
}

/// Every session on the machine, most recently created first.
///
/// A system path, not a request path: boot recovery re-attaches a worker to
/// each live session regardless of who owns it. Nothing reachable from a
/// route may call it.
pub async fn list_sessions_all_owners(store: &DbStore) -> Result<Vec<CodeSession>> {
    entities::code_session::Entity::find()
        .order_by_desc(entities::code_session::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(session_from_row)
        .collect()
}

/// Sessions in one lifecycle state, across every owner.
///
/// A system path, not a request path: boot recovery and the stall sweep act
/// on the whole machine's sessions, the way the chat side's unscoped store
/// handle serves turn workers and retirement scans. Nothing reachable from a
/// route may call it.
pub async fn list_sessions_by_lifecycle_all_owners(
    store: &DbStore,
    lifecycle: CodeSessionLifecycle,
) -> Result<Vec<CodeSession>> {
    entities::code_session::Entity::find()
        .filter(entities::code_session::Column::Lifecycle.eq(lifecycle.as_str().to_owned()))
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(session_from_row)
        .collect()
}

/// Persist mutable session fields. `id`, `workspace_id`, `created_at`,
/// `permission_mode`, execution settings, `attention`, `subagents`, and a
/// missing resume ref stay as stored. These fields have targeted writes so a
/// full-row save from a stale worker cannot clobber a concurrent structured
/// update.
///
/// `spawn_epoch` must be non-decreasing, and `Ended` is terminal: a caller
/// that is not `Ended` cannot overwrite that lifecycle. Returns `false` when
/// nothing was written.
pub async fn save_session(store: &DbStore, session: &CodeSession) -> Result<bool> {
    let mut predicate = Condition::all()
        .add(entities::code_session::Column::Id.eq(session.id.0))
        .add(entities::code_session::Column::Owner.eq(session.owner.as_str()))
        .add(entities::code_session::Column::SpawnEpoch.lte(session.spawn_epoch));
    if session.lifecycle != CodeSessionLifecycle::Ended {
        predicate = predicate.add(
            entities::code_session::Column::Lifecycle
                .ne(CodeSessionLifecycle::Ended.as_str().to_owned()),
        );
    }
    let mut update = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::HarnessKind,
            sea_orm::sea_query::Expr::value(session.harness_kind.as_str().to_owned()),
        )
        .col_expr(
            entities::code_session::Column::HarnessVersion,
            sea_orm::sea_query::Expr::value(session.harness_version.clone()),
        )
        .col_expr(
            entities::code_session::Column::Lifecycle,
            sea_orm::sea_query::Expr::value(session.lifecycle.as_str().to_owned()),
        )
        .col_expr(
            entities::code_session::Column::FenceReason,
            sea_orm::sea_query::Expr::value(match &session.fence_reason {
                Some(reason) => Some(serde_json::to_value(reason)?),
                None => None,
            }),
        )
        .col_expr(
            entities::code_session::Column::ChildPid,
            sea_orm::sea_query::Expr::value(session.child_pid),
        )
        .col_expr(
            entities::code_session::Column::ChildProcessIdentity,
            sea_orm::sea_query::Expr::value(session.child_process_identity.clone()),
        )
        .col_expr(
            entities::code_session::Column::SpawnEpoch,
            sea_orm::sea_query::Expr::value(session.spawn_epoch),
        )
        .col_expr(
            entities::code_session::Column::UnrecognizedEventCount,
            sea_orm::sea_query::Expr::value(session.unrecognized_event_count),
        );
    if let Some(resume_ref) = &session.harness_resume_ref {
        update = update.col_expr(
            entities::code_session::Column::HarnessResumeRef,
            sea_orm::sea_query::Expr::value(Some(resume_ref.clone())),
        );
    }
    let result = update
        .filter(predicate)
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    if result.rows_affected != 1 {
        // Rare, and invisible without a word: distinguish a superseded writer
        // from a row that is simply gone.
        if let Some(current) = get_session(store, &session.owner, session.id).await? {
            tracing::warn!(
                session = %session.id,
                attempted_epoch = session.spawn_epoch,
                current_epoch = current.spawn_epoch,
                attempted_lifecycle = session.lifecycle.as_str(),
                current_lifecycle = current.lifecycle.as_str(),
                "dropping a session write from a superseded or ended code-session row"
            );
        }
        return Ok(false);
    }
    Ok(true)
}

/// Replace one session's attention through the shared row-locked policy.
///
/// `from_user` is the explicit pin or clear path. Automatic callers must pass
/// `false`, which preserves manual attention and reevaluates replacement
/// against the database value instead of a stale session copy.
pub async fn replace_session_attention(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    next: &Attention,
    from_user: bool,
) -> Result<Option<Attention>> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let changed =
        replace_session_attention_on(&transaction, owner, session_id, next, from_user).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(changed)
}

pub(in crate::db) async fn replace_session_attention_on<C>(
    conn: &C,
    owner: &OwnerId,
    session_id: CodeSessionId,
    next: &Attention,
    from_user: bool,
) -> Result<Option<Attention>>
where
    C: ConnectionTrait,
{
    if !acquire_code_session_write_lock(conn, session_id).await? {
        return Ok(None);
    }
    let row = entities::code_session::Entity::find_by_id(session_id.0)
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .one(conn)
        .await
        .map_err(store_err)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let current = session_from_row(row)?.attention;
    if current == *next || (!from_user && !crate::attention::should_replace(&current, next)) {
        return Ok(None);
    }
    let updated = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::AttentionState,
            sea_orm::sea_query::Expr::value(serde_json::to_value(&next.state)?),
        )
        .col_expr(
            entities::code_session::Column::AttentionSource,
            sea_orm::sea_query::Expr::value(next.source.as_str()),
        )
        .filter(entities::code_session::Column::Id.eq(session_id.0))
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "code session {session_id} disappeared while replacing attention"
        )));
    }
    Ok(Some(next.clone()))
}

/// Replace one session's subagent list (decision 52). A targeted write so
/// the event sink never read-modify-writes the whole row against the
/// worker's concurrent full-row saves. Returns `false` when the row is gone
/// or belongs to another owner.
pub async fn set_session_subagents(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    subagents: &[CodeSubagentSummary],
) -> Result<bool> {
    let result = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::Subagents,
            sea_orm::sea_query::Expr::value(if subagents.is_empty() {
                None
            } else {
                Some(serde_json::to_value(subagents)?)
            }),
        )
        .filter(entities::code_session::Column::Id.eq(session_id.0))
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Record the engine-native resume ref once a running turn proves it is safe.
///
/// The event sink learns this value while the turn is still running. A
/// targeted, epoch-fenced write makes it survive a hard restart without
/// letting an outgoing worker restore a stale ref after a reap or relaunch.
pub async fn set_session_harness_resume_ref(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    spawn_epoch: i64,
    resume_ref: &str,
) -> Result<bool> {
    let result = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::HarnessResumeRef,
            sea_orm::sea_query::Expr::value(Some(resume_ref.to_owned())),
        )
        .filter(entities::code_session::Column::Id.eq(session_id.0))
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_session::Column::SpawnEpoch.eq(spawn_epoch))
        .filter(
            entities::code_session::Column::Lifecycle
                .eq(CodeSessionLifecycle::Running.as_str().to_owned()),
        )
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Clear a resume ref that the engine has explicitly rejected.
///
/// A missing ref on [`save_session`] means that the caller holds a stale copy,
/// so only this epoch-fenced write may turn a stored ref back into `NULL`.
pub async fn clear_session_harness_resume_ref(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    spawn_epoch: i64,
) -> Result<bool> {
    let result = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::HarnessResumeRef,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .filter(entities::code_session::Column::Id.eq(session_id.0))
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_session::Column::SpawnEpoch.eq(spawn_epoch))
        .filter(
            entities::code_session::Column::Lifecycle
                .ne(CodeSessionLifecycle::Ended.as_str().to_owned()),
        )
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

pub(super) fn session_from_row(row: entities::code_session::Model) -> Result<CodeSession> {
    let harness_kind = HarnessKind::from_str(&row.harness_kind).ok_or_else(|| {
        AgentError::Store(format!(
            "code_session {} has unknown harness_kind {}",
            row.id, row.harness_kind
        ))
    })?;
    let permission_mode = PermissionMode::from_str(&row.permission_mode).ok_or_else(|| {
        AgentError::Store(format!(
            "code_session {} has unknown permission_mode {}",
            row.id, row.permission_mode
        ))
    })?;
    let kind = CodeSessionKind::from_str(&row.kind).ok_or_else(|| {
        AgentError::Store(format!(
            "code_session {} has unknown kind {}",
            row.id, row.kind
        ))
    })?;
    let lifecycle = CodeSessionLifecycle::from_str(&row.lifecycle).ok_or_else(|| {
        AgentError::Store(format!(
            "code_session {} has unknown lifecycle {}",
            row.id, row.lifecycle
        ))
    })?;
    let source = AttentionSource::from_str(&row.attention_source).ok_or_else(|| {
        AgentError::Store(format!(
            "code_session {} has unknown attention_source {}",
            row.id, row.attention_source
        ))
    })?;
    let state = serde_json::from_value::<AttentionState>(row.attention_state).map_err(|err| {
        AgentError::Store(format!("code_session {} attention_state: {err}", row.id))
    })?;
    let fence_reason = match row.fence_reason {
        Some(value) => Some(serde_json::from_value::<FenceReason>(value).map_err(|err| {
            AgentError::Store(format!("code_session {} fence_reason: {err}", row.id))
        })?),
        None => None,
    };
    let reasoning_effort = match row.reasoning_effort.as_deref() {
        Some(token) => Some(
            crate::model::ReasoningEffort::from_str(token).ok_or_else(|| {
                AgentError::Store(format!(
                    "code_session {} has unknown reasoning_effort {token}",
                    row.id
                ))
            })?,
        ),
        None => None,
    };
    let subagents = match row.subagents {
        Some(value) => {
            serde_json::from_value::<Vec<CodeSubagentSummary>>(value).map_err(|err| {
                AgentError::Store(format!("code_session {} subagents: {err}", row.id))
            })?
        }
        None => Vec::new(),
    };
    Ok(CodeSession {
        id: CodeSessionId(row.id),
        owner: OwnerId::new(&row.owner)?,
        workspace_id: WorkspaceId(row.workspace_id),
        kind,
        harness_kind,
        harness_version: row.harness_version,
        harness_resume_ref: row.harness_resume_ref,
        permission_mode,
        model: row.model,
        reasoning_effort,
        fast_mode: row.fast_mode,
        lifecycle,
        fence_reason,
        child_pid: row.child_pid,
        child_process_identity: row.child_process_identity,
        spawn_epoch: row.spawn_epoch,
        attention: Attention::new(state, source),
        unrecognized_event_count: row.unrecognized_event_count,
        subagents,
        created_at: row.created_at,
    })
}
