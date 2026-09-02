use std::collections::{HashMap, HashSet};

use chrono::Utc;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, QueryTrait, Set, TransactionTrait, TryInsertResult,
};
use serde_json::Value;

use crate::attention::{AttentionSource, AttentionState};
use crate::code::{CodeSessionId, CodeSessionKind, CodeSessionLifecycle, HarnessKind};
use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::{AgentRunId, CallId, ChatId, HostRootId, MessageId, ProjectId, TurnId};
use crate::model::{
    validate_chat_root_projection, validate_chat_root_projection_against_project, Chat,
    ChatRootAttachment, Message, OwnerId, ReasoningEffort, Role, RootAttachmentOrigin,
    ToolCallRecord, TurnRunStatus, MAX_ROOT_ATTACHMENTS,
};
use crate::provider::MessageReasoning;
use crate::storage::{
    ChatTerminalTurnSnapshot, ChatTerminalTurnStatus, ChatToolActivitySnapshot,
    ChatToolActivityStatus, ChatTranscriptSnapshot, DeleteChatOutcome, MessageInvokedSkills,
    MoveChatOutcome, TurnEventAppend,
};
use crate::PermissionMode;

use super::super::{entities, project_from_models, store_err, DbStore};
use super::blob as blob_ops;
use super::chat_image_publication as chat_image_publication_ops;
use super::code::session as code_session_ops;
use super::exec_file_change as exec_file_change_ops;
use super::message_attachment as message_attachment_ops;
use super::turn::canonical_db_timestamp;
use super::{
    acquire_chat_write_lock, acquire_project_write_lock, acquire_turn_write_lock,
    agent_run::insert_foreground_agent_run_on,
};

pub(in crate::db) const MESSAGE_IDENTITY_OWNER_MESSAGE: &str = "message";
pub(in crate::db) const MESSAGE_IDENTITY_OWNER_STEER: &str = "turn_steer";

pub(in crate::db) async fn reserve_message_identity_on<C>(
    conn: &C,
    id: MessageId,
    chat_id: ChatId,
    turn_id: TurnId,
    owner: &str,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    let inserted =
        entities::message_identity::Entity::insert(entities::message_identity::ActiveModel {
            id: Set(id.0),
            chat_id: Set(chat_id.0),
            turn_id: Set(turn_id.0),
            owner: Set(owner.to_owned()),
        })
        .on_conflict(
            OnConflict::column(entities::message_identity::Column::Id)
                .do_nothing()
                .to_owned(),
        )
        .try_insert()
        .exec_without_returning(conn)
        .await
        .map_err(store_err)?;
    Ok(matches!(inserted, TryInsertResult::Inserted(1)))
}

pub(in crate::db) async fn transfer_steer_message_identity_on<C>(
    conn: &C,
    id: MessageId,
    chat_id: ChatId,
    turn_id: TurnId,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    let transferred = entities::message_identity::Entity::update_many()
        .col_expr(
            entities::message_identity::Column::Owner,
            sea_orm::sea_query::Expr::value(MESSAGE_IDENTITY_OWNER_MESSAGE),
        )
        .filter(entities::message_identity::Column::Id.eq(id.0))
        .filter(entities::message_identity::Column::ChatId.eq(chat_id.0))
        .filter(entities::message_identity::Column::TurnId.eq(turn_id.0))
        .filter(entities::message_identity::Column::Owner.eq(MESSAGE_IDENTITY_OWNER_STEER))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(transferred.rows_affected == 1)
}

pub(in crate::db) async fn next_message_seq_on<C>(conn: &C, chat_id: ChatId) -> Result<i64>
where
    C: ConnectionTrait,
{
    entities::message::Entity::find()
        .filter(entities::message::Column::ChatId.eq(chat_id.0))
        .order_by_desc(entities::message::Column::Seq)
        .one(conn)
        .await
        .map_err(store_err)?
        .map_or(Ok(1), |message| {
            message.seq.checked_add(1).ok_or_else(|| {
                AgentError::Store(format!("chat {chat_id} message sequence overflow"))
            })
        })
}

/// The session rows that are conversations: no repo-backed workspace and
/// the internal harness (decision 0048 step 5). A session an external
/// engine drives has no transcript and no turn lane, so every chat read
/// applies this and never sees one.
pub(in crate::db) fn internal_sessions() -> sea_orm::Condition {
    sea_orm::Condition::all()
        .add(entities::code_session::Column::WorkspaceId.is_null())
        .add(entities::code_session::Column::HarnessKind.eq(HarnessKind::Internal.as_str()))
}

pub(in crate::db) async fn create_chat(
    store: &DbStore,
    chat: &Chat,
    owner: Option<&OwnerId>,
) -> Result<()> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let project_roots = load_chat_project_roots(&transaction, chat.project_id, owner).await?;
    validate_chat_attachments(chat, &project_roots)?;
    insert_chat_on(&transaction, chat, owner).await?;
    insert_foreground_agent_run_on(&transaction, chat.id, chat.created_at).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(())
}

pub(in crate::db) async fn create_chat_with_project_defaults(
    store: &DbStore,
    base: &Chat,
    owner: Option<&OwnerId>,
    settings: &[(String, Value)],
) -> Result<Chat> {
    if base.attachment_revision != 0 || !base.root_attachments.is_empty() {
        return Err(AgentError::Store(
            "chat project defaults must be derived from an empty revision-zero projection".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let project_roots = load_chat_project_roots(&transaction, base.project_id, owner).await?;
    let mut chat = base.clone();
    chat.root_attachments = project_roots
        .into_iter()
        .map(|root_id| ChatRootAttachment {
            root_id,
            origin: RootAttachmentOrigin::ProjectDefault,
        })
        .collect();
    if !chat.root_attachments.is_empty() {
        chat.attachment_revision = 1;
    }
    validate_chat_root_projection(&chat).map_err(|message| AgentError::Store(message.into()))?;
    insert_chat_on(&transaction, &chat, owner).await?;
    insert_foreground_agent_run_on(&transaction, chat.id, chat.created_at).await?;
    for (key, value) in settings {
        entities::setting::Entity::insert(entities::setting::ActiveModel {
            key: Set(key.clone()),
            value_json: Set(value.clone()),
        })
        .on_conflict(
            OnConflict::column(entities::setting::Column::Key)
                .update_column(entities::setting::Column::ValueJson)
                .to_owned(),
        )
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(chat)
}

async fn load_chat_project_roots<C>(
    conn: &C,
    project_id: Option<ProjectId>,
    owner: Option<&OwnerId>,
) -> Result<Vec<HostRootId>>
where
    C: ConnectionTrait,
{
    let Some(project_id) = project_id else {
        return Ok(Vec::new());
    };
    if !acquire_project_write_lock(conn, project_id).await? {
        return Err(AgentError::ProjectNotFound(project_id));
    }
    let mut rows = entities::project::Entity::find_by_id(project_id.0)
        .find_with_related(entities::project_root_attachment::Entity)
        .order_by_asc(entities::project_root_attachment::Column::Position)
        .all(conn)
        .await
        .map_err(store_err)?;
    let (model, roots) = rows
        .pop()
        .ok_or_else(|| AgentError::Store(format!("chat project {project_id} does not exist")))?;
    // A project belonging to someone else must be indistinguishable from a
    // missing one (#853).
    if owner.is_some_and(|owner| owner.as_str() != model.owner) {
        return Err(AgentError::ProjectNotFound(project_id));
    }
    Ok(project_from_models(model, roots)?.root_attachments)
}

async fn insert_chat_on<C>(conn: &C, chat: &Chat, owner: Option<&OwnerId>) -> Result<()>
where
    C: ConnectionTrait,
{
    // A conversation is a session the internal engine hosts (decision 0048
    // step 5): no workspace, the internal harness, and the code-owned columns
    // at rest until a session worker attaches. Attention stays derived from
    // `turn_run` (see `ops::chat_attention`); the idle state written here
    // only keeps the column well formed.
    entities::code_session::ActiveModel {
        id: Set(chat.id.0),
        // The local owner rides the column default (which also keeps this
        // insert valid against a pre-owner schema in the upgrade tests); only
        // a named principal writes the column explicitly.
        owner: match owner {
            Some(owner) if !owner.is_local() => Set(owner.as_str().to_owned()),
            _ => sea_orm::ActiveValue::NotSet,
        },
        workspace_id: Set(None),
        kind: Set(CodeSessionKind::Interactive.as_str().to_owned()),
        harness_kind: Set(HarnessKind::Internal.as_str().to_owned()),
        harness_version: Set(None),
        harness_resume_ref: Set(None),
        // `None` is chat's "follow the default at turn time"; the row keeps
        // the null, and the code side reads it as the default.
        permission_mode: Set(chat.permission_mode.map(|mode| mode.as_str().to_owned())),
        permission_mode_revision: Set(0),
        permission_mode_intent: Set(None),
        permission_mode_intent_revision: Set(None),
        permission_mode_intent_epoch: Set(None),
        permission_mode_intent_lifecycle: Set(None),
        model: Set(chat.model.clone()),
        // A newly-created chat has no reasoning override (it is set later via
        // `update_chat_metadata`), so leave the column unset when absent — the DB
        // defaults it to NULL.
        reasoning_effort: match &chat.reasoning_effort {
            Some(effort) => Set(Some(effort.as_str().to_owned())),
            None => sea_orm::ActiveValue::NotSet,
        },
        fast_mode: Set(false),
        lifecycle: Set(CodeSessionLifecycle::Idle.as_str().to_owned()),
        fence_reason: Set(None),
        child_pid: Set(None),
        child_process_identity: Set(None),
        spawn_epoch: Set(0),
        attention_state: Set(serde_json::to_value(AttentionState::Idle)?),
        attention_source: Set(AttentionSource::Lifecycle.as_str().to_owned()),
        unrecognized_event_count: Set(0),
        subagents: Set(None),
        created_at: Set(chat.created_at),
        project_id: Set(chat.project_id.map(|p| p.0)),
        title: Set(chat.title.clone()),
        // Always persist the creation-time choice explicitly. The column's
        // historical off default remains untouched so existing databases and
        // rows are not migrated when the product default changes.
        network_policy: Set(
            serde_json::to_string(&chat.network_policy).map_err(|error| {
                AgentError::Store(format!("could not encode chat network policy: {error}"))
            })?,
        ),
        attachment_revision: Set(chat.attachment_revision),
        memory_incognito: Set(chat.memory_incognito),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    for (position, attachment) in chat.root_attachments.iter().copied().enumerate() {
        entities::chat_root_attachment::ActiveModel {
            chat_id: Set(chat.id.0),
            root_id: Set(*attachment.root_id.as_uuid()),
            position: Set(i32::try_from(position)
                .map_err(|_| AgentError::Store("chat root position exceeds i32".into()))?),
            origin: Set(attachment_origin_to_db(attachment.origin).to_owned()),
        }
        .insert(conn)
        .await
        .map_err(store_err)?;
    }
    Ok(())
}

/// File a conversation under `project_id`, or take it back out with `None`.
///
/// Folder authority is keyed to the identity holding it — a chat inside a
/// project grants across that project, a loose chat only across itself — so a
/// conversation still holding connected folders cannot change identity without
/// stranding the grants the broker issued under the old one. Such a move is
/// refused rather than silently breaking them; the caller disconnects the
/// folders first. Once the conversation is clean, the destination's ordered
/// root defaults are snapshotted exactly as
/// [`create_chat_with_project_defaults`] seeds a new conversation.
pub(in crate::db) async fn move_chat_to_project(
    store: &DbStore,
    id: ChatId,
    project_id: Option<ProjectId>,
    owner: Option<&OwnerId>,
) -> Result<MoveChatOutcome> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    // Destination before conversation, the same order
    // `create_chat_with_project_defaults` takes, so the two cannot deadlock.
    let project_roots = match load_chat_project_roots(&transaction, project_id, owner).await {
        Ok(roots) => roots,
        Err(AgentError::ProjectNotFound(_)) => {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(MoveChatOutcome::ProjectNotFound);
        }
        Err(error) => return Err(error),
    };
    if !acquire_chat_write_lock(&transaction, id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(MoveChatOutcome::ChatNotFound);
    }
    // Someone else's conversation is indistinguishable from an absent one
    // (#853). The owner cannot change under the write lock above.
    let mut query = entities::code_session::Entity::find_by_id(id.0);
    if let Some(owner) = owner {
        query = query
            .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
            .filter(internal_sessions());
    }
    let Some(model) = query.one(&transaction).await.map_err(store_err)? else {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(MoveChatOutcome::ChatNotFound);
    };
    if model.project_id.map(ProjectId) == project_id {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(MoveChatOutcome::Moved);
    }

    let attached = entities::chat_root_attachment::Entity::find()
        .filter(entities::chat_root_attachment::Column::ChatId.eq(id.0))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some();
    if attached {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(MoveChatOutcome::HasConnectedFolders);
    }
    // Product rows can be clear while native authority is not: an in-flight or
    // terminally failed change may still leave the broker attached under the
    // subject this conversation is about to stop being. The latest operation
    // per root has to say detached, exactly as conversation deletion requires.
    let changes = entities::root_attachment_change::Entity::find()
        .filter(entities::root_attachment_change::Column::ChatId.eq(id.0))
        .order_by_desc(entities::root_attachment_change::Column::CreatedAt)
        .order_by_desc(entities::root_attachment_change::Column::Id)
        .all(&transaction)
        .await
        .map_err(store_err)?;
    let mut observed_roots = HashSet::new();
    let attachment_state_unresolved = changes.into_iter().any(|change| {
        observed_roots.insert(change.root_id)
            && (change.phase == "awaiting_broker"
                || change.broker_currently_attached != Some(false))
    });
    if attachment_state_unresolved {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(MoveChatOutcome::FolderChangePending);
    }

    let mut chat = chat_from_models(model, Vec::new())?;
    chat.project_id = project_id;
    chat.root_attachments = project_roots
        .iter()
        .copied()
        .map(|root_id| ChatRootAttachment {
            root_id,
            origin: RootAttachmentOrigin::ProjectDefault,
        })
        .collect();
    // The revision is a CAS counter over the projection, so it only advances
    // when the projection actually changes. It was empty on the way in.
    if !chat.root_attachments.is_empty() {
        chat.attachment_revision += 1;
    }
    validate_chat_root_projection_against_project(&chat, &project_roots)
        .map_err(|message| AgentError::Store(message.into()))?;

    let updated = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::ProjectId,
            sea_orm::sea_query::Expr::value(project_id.map(|project_id| project_id.0)),
        )
        .col_expr(
            entities::code_session::Column::AttachmentRevision,
            sea_orm::sea_query::Expr::value(chat.attachment_revision),
        )
        .filter(entities::code_session::Column::Id.eq(id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "conversation {id} changed while moving it between projects"
        )));
    }
    for (position, attachment) in chat.root_attachments.iter().copied().enumerate() {
        entities::chat_root_attachment::ActiveModel {
            chat_id: Set(id.0),
            root_id: Set(*attachment.root_id.as_uuid()),
            position: Set(i32::try_from(position)
                .map_err(|_| AgentError::Store("chat root position exceeds i32".into()))?),
            origin: Set(attachment_origin_to_db(attachment.origin).to_owned()),
        }
        .insert(&transaction)
        .await
        .map_err(store_err)?;
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(MoveChatOutcome::Moved)
}

pub(in crate::db) async fn set_chat_model(
    store: &DbStore,
    id: ChatId,
    model: Option<String>,
) -> Result<()> {
    entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::Model,
            sea_orm::sea_query::Expr::value(model),
        )
        .filter(entities::code_session::Column::Id.eq(id.0))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

pub(in crate::db) async fn set_chat_title(
    store: &DbStore,
    id: ChatId,
    title: Option<String>,
) -> Result<()> {
    entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::Title,
            sea_orm::sea_query::Expr::value(title),
        )
        .filter(entities::code_session::Column::Id.eq(id.0))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

/// Name a chat only while it is unnamed, reporting whether this call named it.
///
/// The `title IS NULL` predicate is the whole point: it runs in the database, so
/// a user rename that commits first wins even when a derived title was already
/// in flight, and two derived writes cannot both apply.
pub(in crate::db) async fn set_chat_title_if_unset(
    store: &DbStore,
    id: ChatId,
    title: &str,
) -> Result<bool> {
    let result = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::Title,
            sea_orm::sea_query::Expr::value(title),
        )
        .filter(entities::code_session::Column::Id.eq(id.0))
        .filter(entities::code_session::Column::Title.is_null())
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn update_chat_metadata(
    store: &DbStore,
    id: ChatId,
    title: Option<Option<String>>,
    model: Option<Option<String>>,
    reasoning_effort: Option<Option<ReasoningEffort>>,
    permission_mode: Option<Option<PermissionMode>>,
    network_policy: Option<crate::NetworkPolicy>,
    owner: Option<&OwnerId>,
) -> Result<bool> {
    if title.is_none()
        && model.is_none()
        && reasoning_effort.is_none()
        && permission_mode.is_none()
        && network_policy.is_none()
    {
        let mut query = entities::code_session::Entity::find_by_id(id.0);
        if let Some(owner) = owner {
            query = query
                .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
                .filter(internal_sessions());
        }
        return Ok(query.one(&store.conn).await.map_err(store_err)?.is_some());
    }

    let mut update = entities::code_session::Entity::update_many();
    if let Some(title) = title {
        update = update.col_expr(
            entities::code_session::Column::Title,
            sea_orm::sea_query::Expr::value(title),
        );
    }
    if let Some(model) = model {
        update = update.col_expr(
            entities::code_session::Column::Model,
            sea_orm::sea_query::Expr::value(model),
        );
    }
    if let Some(reasoning_effort) = reasoning_effort {
        update = update.col_expr(
            entities::code_session::Column::ReasoningEffort,
            sea_orm::sea_query::Expr::value(
                reasoning_effort.map(|effort| effort.as_str().to_owned()),
            ),
        );
    }
    if let Some(permission_mode) = permission_mode {
        update = update.col_expr(
            entities::code_session::Column::PermissionMode,
            sea_orm::sea_query::Expr::value(permission_mode.map(|mode| mode.as_str().to_owned())),
        );
    }
    if let Some(network_policy) = network_policy {
        let encoded = serde_json::to_string(&network_policy).map_err(|error| {
            AgentError::Store(format!("could not encode chat network policy: {error}"))
        })?;
        update = update.col_expr(
            entities::code_session::Column::NetworkPolicy,
            sea_orm::sea_query::Expr::value(encoded),
        );
    }
    let mut update = update.filter(entities::code_session::Column::Id.eq(id.0));
    if let Some(owner) = owner {
        update = update
            .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
            .filter(internal_sessions());
    }
    let result = update.exec(&store.conn).await.map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Flip one conversation's memory-incognito switch.
///
/// A targeted column write rather than another `update_chat_metadata`
/// parameter: the switch has no tri-state and no sticky default, and keeping
/// it out of that signature spares every caller a positional `None`.
pub(in crate::db) async fn set_chat_memory_incognito(
    store: &DbStore,
    id: ChatId,
    memory_incognito: bool,
    owner: Option<&OwnerId>,
) -> Result<bool> {
    let mut update = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::MemoryIncognito,
            sea_orm::sea_query::Expr::value(memory_incognito),
        )
        .filter(entities::code_session::Column::Id.eq(id.0));
    if let Some(owner) = owner {
        update = update
            .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
            .filter(internal_sessions());
    }
    let result = update.exec(&store.conn).await.map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

pub(in crate::db) async fn get_chat(
    store: &DbStore,
    id: ChatId,
    owner: Option<&OwnerId>,
) -> Result<Option<Chat>> {
    let mut query = entities::code_session::Entity::find_by_id(id.0).filter(internal_sessions());
    if let Some(owner) = owner {
        query = query.filter(entities::code_session::Column::Owner.eq(owner.as_str()));
    }
    let mut rows = query
        .find_with_related(entities::chat_root_attachment::Entity)
        .order_by_asc(entities::chat_root_attachment::Column::Position)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    rows.pop()
        .map(|(model, roots)| chat_from_models(model, roots))
        .transpose()
}

pub(in crate::db) async fn chat_owner(store: &DbStore, id: ChatId) -> Result<Option<OwnerId>> {
    let Some(model) = entities::code_session::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    OwnerId::new(&model.owner).map(Some)
}

pub(in crate::db) async fn list_chats(
    store: &DbStore,
    owner: Option<&OwnerId>,
) -> Result<Vec<Chat>> {
    let mut query = entities::code_session::Entity::find().filter(internal_sessions());
    if let Some(owner) = owner {
        query = query.filter(entities::code_session::Column::Owner.eq(owner.as_str()));
    }
    let mut chats = query
        .find_with_related(entities::chat_root_attachment::Entity)
        .order_by_asc(entities::chat_root_attachment::Column::Position)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|(model, roots)| chat_from_models(model, roots))
        .collect::<Result<Vec<_>>>()?;
    chats.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.id.0.cmp(&left.id.0))
    });
    Ok(chats)
}

/// Remove one fully quiesced conversation and its terminal history.
///
/// Every turn writer takes the chat fence, and all runnable work is rejected
/// before this transaction begins erasing state. Host-root attachment changes
/// are intentionally not treated as ordinary rows: they represent native
/// authority outside this database, so any attached or unreconciled root keeps
/// deletion fail-closed.
pub(in crate::db) async fn delete_chat(
    store: &DbStore,
    chat_id: ChatId,
    owner: Option<&OwnerId>,
) -> Result<DeleteChatOutcome> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(DeleteChatOutcome::NotFound);
    }
    // Someone else's chat is indistinguishable from an absent one (#853). The
    // owner cannot change while the write lock above is held: no owner
    // transfer exists. A session an external engine drives is not a chat,
    // and this cascade is the only deleter of a conversation row: it never
    // reaches one that has a workspace.
    let mut conversation =
        entities::code_session::Entity::find_by_id(chat_id.0).filter(internal_sessions());
    if let Some(owner) = owner {
        conversation =
            conversation.filter(entities::code_session::Column::Owner.eq(owner.as_str()));
    }
    if conversation
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_none()
    {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(DeleteChatOutcome::NotFound);
    }

    let roots_attached = entities::chat_root_attachment::Entity::find()
        .filter(entities::chat_root_attachment::Column::ChatId.eq(chat_id.0))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some();
    if roots_attached {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(DeleteChatOutcome::RootsAttached);
    }

    // A terminal `failed` attachment can intentionally retain an unknown broker
    // observation. Looking only for `awaiting_broker` would let a failed attach
    // with no final broker state erase the product record while native authority
    // might still exist. The latest operation per root must conclusively say
    // detached; older attach receipts are superseded by a later detach receipt.
    let changes = entities::root_attachment_change::Entity::find()
        .filter(entities::root_attachment_change::Column::ChatId.eq(chat_id.0))
        .order_by_desc(entities::root_attachment_change::Column::CreatedAt)
        .order_by_desc(entities::root_attachment_change::Column::Id)
        .all(&transaction)
        .await
        .map_err(store_err)?;
    let mut observed_roots = HashSet::new();
    let attachment_state_unresolved = changes.into_iter().any(|change| {
        observed_roots.insert(change.root_id)
            && (change.phase == "awaiting_broker"
                || change.broker_currently_attached != Some(false))
    });
    if attachment_state_unresolved {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(DeleteChatOutcome::RootAttachmentStateUnresolved);
    }

    let active_turn = entities::turn_run::Entity::find()
        .filter(entities::turn_run::Column::ChatId.eq(chat_id.0))
        .filter(entities::turn_run::Column::Status.is_not_in([
            TurnRunStatus::Completed.as_str(),
            TurnRunStatus::Failed.as_str(),
            TurnRunStatus::Cancelled.as_str(),
        ]))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some();
    let active_sandbox = entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::ChatId.eq(chat_id.0))
        .filter(entities::agent_run::Column::Tier.eq("background"))
        .filter(entities::agent_run::Column::Status.is_not_in(["completed", "failed", "cancelled"]))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some();
    if active_turn || active_sandbox {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(DeleteChatOutcome::ActiveWork);
    }

    // Sources are product state owned by the conversation. Their
    // content-addressed blobs are cleaned asynchronously after this atomic
    // operational transaction commits.
    let documents = entities::document::Entity::find()
        .filter(entities::document::Column::ChatId.eq(chat_id.0))
        .all(&transaction)
        .await
        .map_err(store_err)?;
    for document in documents {
        let deleted = entities::document::Entity::delete_many()
            .filter(entities::document::Column::Id.eq(document.id))
            .filter(entities::document::Column::ChatId.eq(chat_id.0))
            .exec(&transaction)
            .await
            .map_err(store_err)?;
        if deleted.rows_affected != 1 {
            return Err(AgentError::Store(format!(
                "document {} changed while deleting chat {chat_id}",
                document.id
            )));
        }
        if let Some(blob_id) = document.source_blob_id {
            blob_ops::enqueue_on(&transaction, blob_id).await?;
        }
    }

    // Published-image reservations and message attachments are the
    // conversation's image blob references. Dropping them here only creates
    // retirement candidates: blob ids are content-derived, so another
    // conversation may still reserve or attach the same bytes. The retirement
    // claim performs the authoritative union reference check and cancels a
    // candidate that is still live, exactly as it does for documents.
    let mut image_blob_ids =
        chat_image_publication_ops::list_chat_blob_ids_on(&transaction, chat_id).await?;
    chat_image_publication_ops::delete_for_chat_on(&transaction, chat_id).await?;
    image_blob_ids
        .extend(message_attachment_ops::list_chat_blob_ids_on(&transaction, chat_id).await?);
    // Attachments must go before the message rows they point at, which the
    // ordering below depends on.
    message_attachment_ops::delete_for_chat_on(&transaction, chat_id).await?;
    image_blob_ids.sort_unstable();
    image_blob_ids.dedup();
    for blob_id in image_blob_ids {
        blob_ops::enqueue_on(&transaction, blob_id).await?;
    }

    // The file-change journal is the third, on the same terms: deleting the
    // conversation retracts the undo it offered, so the prior copies it was
    // holding become retirement candidates.
    let snapshot_blob_ids =
        exec_file_change_ops::list_chat_blob_ids_on(&transaction, chat_id).await?;
    exec_file_change_ops::delete_for_chat_on(&transaction, chat_id).await?;
    for blob_id in snapshot_blob_ids {
        blob_ops::enqueue_on(&transaction, blob_id).await?;
    }

    // Delete dependency leaves before their parent lifecycle rows. These
    // tables intentionally use restrictive foreign keys to make normal state
    // machine mistakes visible; conversation deletion is the explicit terminal
    // owner that can erase their complete, quiesced graph in one transaction.
    entities::turn_client_wait::Entity::delete_many()
        .filter(entities::turn_client_wait::Column::ChatId.eq(chat_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    entities::turn_agent_run_wait_member::Entity::delete_many()
        .filter(entities::turn_agent_run_wait_member::Column::ChatId.eq(chat_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    entities::turn_agent_run_wait_set::Entity::delete_many()
        .filter(entities::turn_agent_run_wait_set::Column::ChatId.eq(chat_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    entities::turn_steer::Entity::delete_many()
        .filter(entities::turn_steer::Column::ChatId.eq(chat_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    entities::sandbox_spawn_checkpoint::Entity::delete_many()
        .filter(entities::sandbox_spawn_checkpoint::Column::ChatId.eq(chat_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    // The task plan restricts against the chat, its last-writing turn, and the
    // call that wrote it, so it goes before all three.
    entities::task_plan::Entity::delete_many()
        .filter(entities::task_plan::Column::ChatId.eq(chat_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    entities::tool_call::Entity::delete_many()
        .filter(entities::tool_call::Column::ChatId.eq(chat_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    // Approval-bearing tool calls own immutable receipts in the event journal.
    // Remove those references before deleting their journal rows.
    entities::code_event::Entity::delete_many()
        .filter(entities::code_event::Column::SessionId.eq(chat_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    // A background run's plan restricts against both the run and the checkpoint
    // that wrote it, so it goes before the sandbox call rows and the runs.
    entities::agent_run_task_plan::Entity::delete_many()
        .filter(
            entities::agent_run_task_plan::Column::AgentRunId.in_subquery(
                entities::agent_run::Entity::find()
                    .select_only()
                    .column(entities::agent_run::Column::Id)
                    .filter(entities::agent_run::Column::ChatId.eq(chat_id.0))
                    .into_query(),
            ),
        )
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    entities::sandbox_tool_call::Entity::delete_many()
        .filter(entities::sandbox_tool_call::Column::ChatId.eq(chat_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    entities::agent_run_inbox::Entity::delete_many()
        .filter(entities::agent_run_inbox::Column::ChatId.eq(chat_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    // Background runs own host workspaces named `agent-run-<id>`. Capture the
    // ids here, before the rows go, so the deletion path can destroy those
    // directories immediately instead of waiting on the orphan reaper.
    let background_run_ids = entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::ChatId.eq(chat_id.0))
        .filter(entities::agent_run::Column::Tier.eq("background"))
        .all(&transaction)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|run| AgentRunId::from(run.id))
        .collect::<Vec<_>>();
    let agent_runs = entities::agent_run::Entity::find()
        .select_only()
        .column(entities::agent_run::Column::Id)
        .filter(entities::agent_run::Column::ChatId.eq(chat_id.0))
        .into_query();
    entities::agent_run_cancellation::Entity::delete_many()
        .filter(
            entities::agent_run_cancellation::Column::AgentRunId.in_subquery(agent_runs.clone()),
        )
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    entities::agent_run_result::Entity::delete_many()
        .filter(entities::agent_run_result::Column::AgentRunId.in_subquery(agent_runs.clone()))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    entities::turn_failure::Entity::delete_many()
        .filter(
            entities::turn_failure::Column::TurnId.in_subquery(
                entities::turn_run::Entity::find()
                    .select_only()
                    .column(entities::turn_run::Column::Id)
                    .filter(entities::turn_run::Column::ChatId.eq(chat_id.0))
                    .into_query(),
            ),
        )
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    entities::turn_claim::Entity::delete_many()
        .filter(
            entities::turn_claim::Column::TurnId.in_subquery(
                entities::turn_run::Entity::find()
                    .select_only()
                    .column(entities::turn_run::Column::Id)
                    .filter(entities::turn_run::Column::ChatId.eq(chat_id.0))
                    .into_query(),
            ),
        )
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    entities::turn_run::Entity::delete_many()
        .filter(entities::turn_run::Column::ChatId.eq(chat_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    entities::message_identity::Entity::delete_many()
        .filter(entities::message_identity::Column::ChatId.eq(chat_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    // A context checkpoint references its inclusive source message. It is
    // provider-only state, so delete it before the source message rather than
    // leaving a checkpoint that could outlive this conversation's history.
    entities::context_checkpoint::Entity::delete_many()
        .filter(entities::context_checkpoint::Column::ChatId.eq(chat_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    entities::message::Entity::delete_many()
        .filter(entities::message::Column::ChatId.eq(chat_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    entities::agent_run_claim::Entity::delete_many()
        .filter(entities::agent_run_claim::Column::AgentRunId.in_subquery(agent_runs))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    entities::agent_run::Entity::delete_many()
        .filter(entities::agent_run::Column::ChatId.eq(chat_id.0))
        .filter(entities::agent_run::Column::Depth.eq(1))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    entities::agent_run::Entity::delete_many()
        .filter(entities::agent_run::Column::ChatId.eq(chat_id.0))
        .filter(entities::agent_run::Column::Depth.eq(0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    entities::root_attachment_change::Entity::delete_many()
        .filter(entities::root_attachment_change::Column::ChatId.eq(chat_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    // A conversation the internal engine has hosted carries code-side rows
    // under the same id (its journal, turns, approvals, image reservations).
    // They key the session row, so they go first, and their image blobs join
    // the retirement candidates on the same terms as the chat-side ones.
    for blob_id in
        code_session_ops::delete_session_dependents_on(&transaction, CodeSessionId(chat_id.0))
            .await?
    {
        blob_ops::enqueue_on(&transaction, blob_id).await?;
    }
    entities::code_session::Entity::delete_by_id(chat_id.0)
        .exec(&transaction)
        .await
        .map_err(store_err)?;

    transaction.commit().await.map_err(store_err)?;
    Ok(DeleteChatOutcome::Deleted { background_run_ids })
}

/// Read the visible transcript and cursor for future journal replay under the
/// same per-chat fence. Every message/event writer takes this fence, so no turn
/// can commit between the two reads. Only a terminal event is represented by
/// the durable assistant transcript; a later active turn's streamed deltas
/// must remain after the cursor for the renderer to reconstruct it on replay.
pub(in crate::db) async fn get_chat_transcript(
    store: &DbStore,
    chat_id: ChatId,
    owner: Option<&OwnerId>,
) -> Result<Option<ChatTranscriptSnapshot>> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    // Someone else's transcript is indistinguishable from an absent one (#853).
    if let Some(owner) = owner {
        let owned = entities::code_session::Entity::find_by_id(chat_id.0)
            .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
            .filter(internal_sessions())
            .one(&transaction)
            .await
            .map_err(store_err)?
            .is_some();
        if !owned {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(None);
        }
    }
    let messages = list_messages_on(&transaction, chat_id).await?;
    let message_attachments =
        super::message_attachment::list_for_chat_on(&transaction, chat_id).await?;
    let message_document_attachments =
        super::message_document_attachment::list_for_chat_on(&transaction, chat_id).await?;
    let citations = super::citation::list_snapshots_on(&transaction, chat_id).await?;
    let message_invoked_skills = list_message_invoked_skills_on(&transaction, chat_id).await?;
    let terminal_turns = list_terminal_turns_on(&transaction, chat_id, &messages).await?;
    let tool_activity = list_terminal_tool_activity_on(&transaction, chat_id).await?;
    let last_event_seq = terminal_event_cursor_on(&transaction, chat_id).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(ChatTranscriptSnapshot {
        messages,
        message_attachments,
        message_document_attachments,
        citations,
        message_invoked_skills,
        terminal_turns,
        tool_activity,
        last_event_seq,
    }))
}

/// Pair every user message that invoked skills with the list it invoked.
///
/// Two authorities, because invocation is scoped to one message: a turn's row
/// owns what its opening message named, and a steer's row owns what that one
/// instruction named. Reading them back beats copying the list onto `message`,
/// where the duplicate could disagree with the list the model was actually
/// given. A turn that invoked nothing contributes no entry.
async fn list_message_invoked_skills_on<C>(
    conn: &C,
    chat_id: ChatId,
) -> Result<Vec<MessageInvokedSkills>>
where
    C: ConnectionTrait,
{
    let turns = entities::turn_run::Entity::find()
        .filter(entities::turn_run::Column::ChatId.eq(chat_id.0))
        .all(conn)
        .await
        .map_err(store_err)?;
    let mut invoked = Vec::new();
    for turn in &turns {
        let skills = super::turn::invoked_skills_from_model(turn)?;
        if !skills.is_empty() {
            invoked.push(MessageInvokedSkills {
                message_id: MessageId(turn.input_message_id),
                skills,
            });
        }
    }
    let steers = entities::turn_steer::Entity::find()
        .filter(entities::turn_steer::Column::ChatId.eq(chat_id.0))
        .filter(entities::turn_steer::Column::MessageId.is_not_null())
        .all(conn)
        .await
        .map_err(store_err)?;
    for steer in &steers {
        // Guarded by the query, but a steer with no applied message has nothing
        // in the transcript to attach to either way.
        let Some(message_id) = steer.message_id else {
            continue;
        };
        let skills = super::turn::steer::invoked_skills_from_steer(steer)?;
        if !skills.is_empty() {
            invoked.push(MessageInvokedSkills {
                message_id: MessageId(message_id),
                skills,
            });
        }
    }
    Ok(invoked)
}

/// Rebuild the visible stream and outcome of every terminal turn.
///
/// The journal holds every delta a chat has ever produced, tool arguments
/// included, so this deliberately filters on the payload's variant tag in SQL
/// rather than deserializing the chat's whole event history. Completed prose
/// still comes from its committed message. A cancellation after an intermediate
/// step committed points at the last assistant message from that turn; only a
/// genuinely message-less terminal turn retains its streamed text here.
async fn list_terminal_turns_on<C>(
    conn: &C,
    chat_id: ChatId,
    messages: &[Message],
) -> Result<Vec<ChatTerminalTurnSnapshot>>
where
    C: ConnectionTrait,
{
    let turns = entities::turn_run::Entity::find()
        .filter(entities::turn_run::Column::ChatId.eq(chat_id.0))
        .filter(entities::turn_run::Column::Status.is_in([
            TurnRunStatus::Completed.as_str(),
            TurnRunStatus::Failed.as_str(),
            TurnRunStatus::Cancelled.as_str(),
        ]))
        .order_by_asc(entities::turn_run::Column::FinishedAt)
        .order_by_asc(entities::turn_run::Column::Id)
        .all(conn)
        .await
        .map_err(store_err)?;
    if turns.is_empty() {
        return Ok(Vec::new());
    }

    let last_assistant_message_by_turn = messages
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .fold(HashMap::new(), |mut by_turn, message| {
            by_turn.insert(message.turn_id, message.id);
            by_turn
        });
    let mut snapshots = Vec::with_capacity(turns.len());
    let mut index_of = HashMap::with_capacity(turns.len());
    for turn in turns {
        let Some(finished_at) = turn.finished_at else {
            // Terminal rows are constrained to have a finish time. If legacy
            // corruption violates that, it cannot be placed honestly.
            continue;
        };
        let status = match turn.status.as_str() {
            status if status == TurnRunStatus::Completed.as_str() => {
                ChatTerminalTurnStatus::Completed
            }
            status if status == TurnRunStatus::Failed.as_str() => ChatTerminalTurnStatus::Failed,
            status if status == TurnRunStatus::Cancelled.as_str() => {
                ChatTerminalTurnStatus::Cancelled
            }
            _ => continue,
        };
        let invoked_skills = super::turn::invoked_skills_from_model(&turn)?;
        let usage = super::turn::usage_from_turn_model(&turn)?;
        let message_id = turn.output_message_id.map(MessageId).or_else(|| {
            matches!(&status, ChatTerminalTurnStatus::Cancelled)
                .then(|| {
                    last_assistant_message_by_turn
                        .get(&TurnId(turn.id))
                        .copied()
                })
                .flatten()
        });
        index_of.insert(turn.id, snapshots.len());
        snapshots.push(ChatTerminalTurnSnapshot {
            turn_id: TurnId(turn.id),
            message_id,
            status,
            partial_content: String::new(),
            reasoning: String::new(),
            refusal: None,
            failure_kind: turn.last_error_code,
            failure_detail: turn.last_error_detail,
            model: turn.model,
            invoked_skills,
            usage,
            voice_input_used: turn.voice_input_used,
            finished_at,
        });
    }
    if snapshots.is_empty() {
        return Ok(snapshots);
    }

    // These tags are SQL literals rather than bound values on purpose.
    // sea-query does not rewrite `?` inside a custom expression for PostgreSQL
    // (`?` is one of its JSONB operators), so a placeholder reaches the server
    // verbatim while SQLite accepts it. The values are crate constants, never
    // caller input.
    let tag_matches = match conn.get_database_backend() {
        DatabaseBackend::Postgres => format!(
            "event ->> 'type' IN ('{TEXT_DELTA_TAG}', '{REASONING_DELTA_TAG}', '{TURN_REFUSED_TAG}')"
        ),
        _ => format!(
            "json_extract(event, '$.type') IN ('{TEXT_DELTA_TAG}', '{REASONING_DELTA_TAG}', '{TURN_REFUSED_TAG}')"
        ),
    };
    let events = entities::code_event::Entity::find()
        .filter(entities::code_event::Column::SessionId.eq(chat_id.0))
        .filter(sea_orm::sea_query::Expr::cust(tag_matches))
        .order_by_asc(entities::code_event::Column::Seq)
        .all(conn)
        .await
        .map_err(store_err)?;

    for event in events {
        let Some(turn_id) = event.turn_id else {
            continue;
        };
        let Some(index) = index_of.get(&turn_id).copied() else {
            continue;
        };
        match crate::chat_journal::decode_chat_event_required(event.event)? {
            AgentEvent::TextDelta { text } if snapshots[index].message_id.is_none() => {
                snapshots[index].partial_content.push_str(&text);
            }
            AgentEvent::ReasoningDelta { text } => snapshots[index].reasoning.push_str(&text),
            AgentEvent::TurnRefused { refusal, .. } => snapshots[index].refusal = Some(refusal),
            _ => {}
        }
    }
    Ok(snapshots)
}

/// Serialized `CodeEvent` tags of the chat rows that rebuild terminal turn
/// presentation and tool-call bodies (`crate::chat_journal`).
/// Journaled bytes are a persisted shape, so the transcript test pins them
/// rather than trusting the enum names to stay in step by inspection.
const TEXT_DELTA_TAG: &str = "assistant_delta";
const REASONING_DELTA_TAG: &str = "reasoning_delta";
const TURN_REFUSED_TAG: &str = "turn_refused";
const TOOL_CALL_ARGS_DELTA_TAG: &str = "tool_args_delta";
const TOOL_CALL_COMPLETED_TAG: &str = "tool_completed";

/// Read only tool calls whose owning foreground turn has reached a terminal
/// state. A live call is reconstructed from the event journal instead: showing
/// it in the snapshot before its event is committed would make reconnecting
/// renderers duplicate or skip activity.
async fn list_terminal_tool_activity_on<C>(
    conn: &C,
    chat_id: ChatId,
) -> Result<Vec<ChatToolActivitySnapshot>>
where
    C: ConnectionTrait,
{
    let terminal_turn_ids: HashSet<_> = entities::turn_run::Entity::find()
        .filter(entities::turn_run::Column::ChatId.eq(chat_id.0))
        .filter(entities::turn_run::Column::Status.is_in([
            TurnRunStatus::Completed.as_str(),
            TurnRunStatus::Failed.as_str(),
            TurnRunStatus::Cancelled.as_str(),
        ]))
        .all(conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|turn| turn.id)
        .collect();
    entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::ChatId.eq(chat_id.0))
        .filter(entities::tool_call::Column::Status.is_in([
            crate::model::ToolCallStatus::Completed.as_str(),
            crate::model::ToolCallStatus::Failed.as_str(),
            crate::model::ToolCallStatus::Cancelled.as_str(),
        ]))
        .order_by_asc(entities::tool_call::Column::HistoryOrder)
        .order_by_asc(entities::tool_call::Column::Id)
        .all(conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .filter(|call| terminal_turn_ids.contains(&call.turn_id))
        .map(|model| {
            // Read off the model before it is narrowed to a record: the
            // projection is renderer state and has no place on the canonical
            // tool-call record the rest of the store passes around.
            let stored_preview = model.result_preview.clone();
            super::client_execution::tool_call_from_model(model)
                .map(|call| tool_activity_from_call(call, stored_preview))
        })
        .collect::<Result<Vec<_>>>()
}

fn tool_activity_from_call(
    call: ToolCallRecord,
    stored_preview: Option<serde_json::Value>,
) -> ChatToolActivitySnapshot {
    let status = match call.status {
        crate::model::ToolCallStatus::Completed => ChatToolActivityStatus::Completed,
        crate::model::ToolCallStatus::Failed
            if call.error_code.as_deref()
                == Some(crate::tool::ToolErrorCategory::UserDeclined.as_str()) =>
        {
            ChatToolActivityStatus::Denied
        }
        crate::model::ToolCallStatus::Failed => ChatToolActivityStatus::Failed,
        crate::model::ToolCallStatus::Cancelled => ChatToolActivityStatus::Cancelled,
        crate::model::ToolCallStatus::Pending => {
            unreachable!("pending tool calls are excluded from durable activity")
        }
    };
    // A retained projection is what the call actually produced, so it outranks
    // anything rebuilt from the failure code — that fallback exists for calls
    // resolved before projections were retained at all.
    let (result, result_unreadable) = match stored_preview {
        Some(stored) => match serde_json::from_value(stored) {
            Ok(preview) => (Some(preview), false),
            // The union is allowed to move. A row that no longer parses is a
            // result this build cannot show, which is a different fact from
            // this call having produced none.
            Err(_) => (None, true),
        },
        None => (
            crate::preview::ToolResultPreview::from_stored_error(
                &call.name,
                call.error_code.as_deref(),
            ),
            false,
        ),
    };
    ChatToolActivitySnapshot {
        call_id: call.id,
        tool: crate::RendererToolName::from(call.name.as_str()),
        action: crate::preview::ToolActionPreview::build(&call.name, &call.arguments),
        result,
        result_unreadable,
        background_agent_run_id: (call.name == crate::agent_tools::SPAWN_SANDBOX_AGENT_TOOL)
            .then(|| crate::AgentRunId::sandbox_for_spawn_call(call.id)),
        status,
        started_at: call.created_at,
        finished_at: call.resolved_at,
    }
}

/// The tool name is canonical but still not renderer-safe: an unknown name can
/// leak provider, extension, or local capability details. Historical cards name
/// only tools on this list and fold anything else to `other`, which is the same
/// vocabulary and the same fold the live event projection uses.
///
/// Returning the name rather than its copy is deliberate. The renderer already
/// owns the wording for a live call, so sending prose here meant maintaining a
/// second copy of it plus an inverse lookup to get back to a name — and a copy
/// change on either side silently broke hydration.
async fn terminal_event_cursor_on<C>(conn: &C, chat_id: ChatId) -> Result<i64>
where
    C: ConnectionTrait,
{
    entities::code_event::Entity::find()
        .filter(entities::code_event::Column::SessionId.eq(chat_id.0))
        .filter(entities::code_event::Column::Terminal.eq(true))
        .order_by_desc(entities::code_event::Column::Seq)
        .one(conn)
        .await
        .map_err(store_err)?
        .map_or(Ok(0), |event| Ok(event.seq))
}

pub(in crate::db) async fn append_message(store: &DbStore, message: &Message) -> Result<()> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, message.chat_id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "chat {} does not exist",
            message.chat_id
        )));
    }
    if !reserve_message_identity_on(
        &transaction,
        message.id,
        message.chat_id,
        message.turn_id,
        MESSAGE_IDENTITY_OWNER_MESSAGE,
    )
    .await?
    {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "message identity {} is already reserved",
            message.id
        )));
    }
    let seq = next_message_seq_on(&transaction, message.chat_id).await?;
    let active = entities::message::ActiveModel {
        id: Set(message.id.0),
        chat_id: Set(message.chat_id.0),
        turn_id: Set(message.turn_id.0),
        seq: Set(seq),
        role: Set(role_to_db(message.role).to_string()),
        content: Set(message.content.clone()),
        llm_content: Set(message.llm_content.clone()),
        reasoning: Set(reasoning_to_db(&message.reasoning)),
        turn_lease_token: Set(None),
        created_at: Set(message.created_at),
    };
    if let Err(error) = active.insert(&transaction).await {
        transaction.rollback().await.map_err(store_err)?;
        return Err(store_err(error));
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(())
}

pub(in crate::db) async fn list_messages(store: &DbStore, chat_id: ChatId) -> Result<Vec<Message>> {
    list_messages_on(&store.conn, chat_id).await
}

/// Output message ids of the chat's cancelled turns — the partial prose a
/// cancel committed (#1182), which context assembly annotates as interrupted.
pub(in crate::db) async fn list_cancelled_output_message_ids(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<MessageId>> {
    let turns = entities::turn_run::Entity::find()
        .filter(entities::turn_run::Column::ChatId.eq(chat_id.0))
        .filter(entities::turn_run::Column::Status.eq(TurnRunStatus::Cancelled.as_str()))
        .filter(entities::turn_run::Column::OutputMessageId.is_not_null())
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(turns
        .into_iter()
        .filter_map(|turn| turn.output_message_id.map(MessageId))
        .collect())
}

async fn list_messages_on<C>(conn: &C, chat_id: ChatId) -> Result<Vec<Message>>
where
    C: ConnectionTrait,
{
    entities::message::Entity::find()
        .filter(entities::message::Column::ChatId.eq(chat_id.0))
        .order_by_asc(entities::message::Column::Seq)
        .all(conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(message_from_model)
        .collect()
}

pub(in crate::db) async fn list_tool_calls(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<ToolCallRecord>> {
    entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::ChatId.eq(chat_id.0))
        .order_by_asc(entities::tool_call::Column::HistoryOrder)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(super::client_execution::tool_call_from_model)
        .collect()
}

pub(in crate::db) async fn append_event(
    store: &DbStore,
    chat_id: ChatId,
    event: &AgentEvent,
) -> Result<i64> {
    // Serialize sequence allocation on the durable chat row. This is also the
    // lock used by turn acceptance, so independently running servers cannot
    // race a journal append with the next turn's admission.
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        return Err(AgentError::Store(format!("chat {chat_id} does not exist")));
    }
    if entities::turn_run::Entity::find()
        .filter(entities::turn_run::Column::ChatId.eq(chat_id.0))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some()
    {
        return Err(AgentError::Store(format!(
            "chat {chat_id} uses durable turns; append through an exact claim"
        )));
    }

    let seq = append_event_on(&transaction, chat_id, None, None, None, None, event).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(seq)
}

/// Append an event that belongs to the chat rather than to a turn.
///
/// Sequence allocation takes the same chat write lock turn acceptance takes, so
/// a maintenance event and the next turn's admission cannot race for a number.
/// A terminal event is refused outright: those resolve a turn, and a turn-less
/// row could not name the one it resolved.
pub(in crate::db) async fn append_chat_event(
    store: &DbStore,
    chat_id: ChatId,
    event: &AgentEvent,
) -> Result<i64> {
    if is_terminal_event(event) {
        return Err(AgentError::Store(format!(
            "chat {chat_id} cannot journal a terminal event outside a turn"
        )));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        return Err(AgentError::Store(format!("chat {chat_id} does not exist")));
    }
    let seq = append_event_on(&transaction, chat_id, None, None, None, None, event).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(seq)
}

pub(in crate::db) async fn append_turn_event(
    store: &DbStore,
    chat_id: ChatId,
    turn_id: TurnId,
    lease_token: uuid::Uuid,
    attempt_event_ordinal: i32,
    now: chrono::DateTime<Utc>,
    event: &AgentEvent,
) -> Result<Option<i64>> {
    let entry = TurnEventAppend {
        attempt_event_ordinal,
        event: event.clone(),
    };
    Ok(append_turn_events(
        store,
        chat_id,
        turn_id,
        lease_token,
        now,
        std::slice::from_ref(&entry),
    )
    .await?
    .map(|seqs| seqs[0]))
}

/// Journal a run of nonterminal turn events under one transaction.
///
/// The chat write lock, turn write lock, and lease check are taken once for the
/// whole run, so a streaming turn pays one commit for however many text deltas
/// arrived together instead of one per delta. Every entry is still identified
/// by `(lease_token, attempt_event_ordinal)`: an entry whose identity already
/// exists with the same payload recovers its original sequence, and one that
/// exists with different data is an error. A batch that mixes recovered and
/// fresh entries validates the lease before inserting the fresh ones, exactly
/// as a single append would.
pub(in crate::db) async fn append_turn_events(
    store: &DbStore,
    chat_id: ChatId,
    turn_id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
    events: &[TurnEventAppend],
) -> Result<Option<Vec<i64>>> {
    if turn_id.0.is_nil() {
        return Err(AgentError::Store("event turn id must not be nil".into()));
    }
    if lease_token.is_nil() {
        return Err(AgentError::Store(
            "event lease token must not be nil".into(),
        ));
    }
    if events.is_empty() {
        return Err(AgentError::Store(
            "turn event batch must carry at least one event".into(),
        ));
    }
    let mut previous_ordinal = None;
    for entry in events {
        let attempt_event_ordinal = entry.attempt_event_ordinal;
        if !(1..i32::MAX).contains(&attempt_event_ordinal) {
            return Err(AgentError::Store(
                "attempt event ordinal must be positive and below the terminal slot".into(),
            ));
        }
        if previous_ordinal.is_some_and(|previous| attempt_event_ordinal <= previous) {
            return Err(AgentError::Store(
                "turn event batch ordinals must ascend".into(),
            ));
        }
        previous_ordinal = Some(attempt_event_ordinal);
        if is_terminal_event(&entry.event) {
            return Err(AgentError::Store(
                "terminal turn events must be committed by turn resolution".into(),
            ));
        }
        if let AgentEvent::TurnStarted {
            turn_id: payload_turn_id,
        } = &entry.event
        {
            if *payload_turn_id != turn_id {
                return Err(AgentError::Store(format!(
                    "turn-started event names {payload_turn_id}, not authoritative turn {turn_id}"
                )));
            }
        }
    }
    let now = canonical_db_timestamp(now)?;

    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        return Err(AgentError::Store(format!("chat {chat_id} does not exist")));
    }
    if !acquire_turn_write_lock(&transaction, turn_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }

    let existing = entities::code_event::Entity::find()
        .filter(entities::code_event::Column::LeaseToken.eq(lease_token))
        .filter(
            entities::code_event::Column::AttemptEventOrdinal
                .is_in(events.iter().map(|entry| entry.attempt_event_ordinal)),
        )
        .all(&transaction)
        .await
        .map_err(store_err)?;
    let mut recovered: HashMap<i32, i64> = HashMap::with_capacity(existing.len());
    for row in existing {
        let Some(attempt_event_ordinal) = row.attempt_event_ordinal else {
            continue;
        };
        let Some(entry) = events
            .iter()
            .find(|entry| entry.attempt_event_ordinal == attempt_event_ordinal)
        else {
            continue;
        };
        let payload = crate::chat_journal::decode_chat_event_required(row.event.clone())?;
        if row.session_id != chat_id.0
            || row.turn_id != Some(turn_id.0)
            || row.lease_token != Some(lease_token)
            || row.terminal
            || payload != entry.event
        {
            return Err(AgentError::Store(format!(
                "turn event identity ({lease_token}, {attempt_event_ordinal}) was reused with different data"
            )));
        }
        recovered.insert(attempt_event_ordinal, row.seq);
    }
    if recovered.len() == events.len() {
        let seqs = events
            .iter()
            .map(|entry| recovered[&entry.attempt_event_ordinal])
            .collect();
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(seqs));
    }

    let Some(claim) = entities::turn_claim::Entity::find_by_id(lease_token)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .filter(|claim| claim.turn_id == turn_id.0)
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let Some(turn) = entities::turn_run::Entity::find_by_id(turn_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if turn.chat_id != chat_id.0 {
        return Err(AgentError::Store(format!(
            "turn {turn_id} does not belong to chat {chat_id}"
        )));
    }
    if turn.status != TurnRunStatus::Running.as_str()
        || turn.attempt_count != claim.attempt_count
        || turn.claim_count != claim.claim_count
        || turn.lease_token != Some(lease_token)
        || turn
            .lease_expires_at
            .is_none_or(|lease_expires_at| lease_expires_at <= now)
        || turn.updated_at > now
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }

    let mut next_seq = next_event_seq(&transaction, chat_id).await?;
    let mut seqs = Vec::with_capacity(events.len());
    for entry in events {
        if let Some(seq) = recovered.get(&entry.attempt_event_ordinal) {
            seqs.push(*seq);
            continue;
        }
        let seq = next_seq;
        next_seq = next_seq.checked_add(1).ok_or_else(|| {
            AgentError::Store(format!("event sequence exhausted for chat {chat_id}"))
        })?;
        insert_event_row(
            &transaction,
            chat_id,
            seq,
            Some(turn_id),
            Some(lease_token),
            Some(entry.attempt_event_ordinal),
            None,
            &entry.event,
        )
        .await?;
        seqs.push(seq);
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(seqs))
}

pub(in crate::db::ops) async fn append_event_on<C>(
    conn: &C,
    chat_id: ChatId,
    turn_id: Option<TurnId>,
    lease_token: Option<uuid::Uuid>,
    attempt_event_ordinal: Option<i32>,
    scan_token: Option<uuid::Uuid>,
    event: &AgentEvent,
) -> Result<i64>
where
    C: ConnectionTrait,
{
    let seq = next_event_seq(conn, chat_id).await?;
    insert_event_row(
        conn,
        chat_id,
        seq,
        turn_id,
        lease_token,
        attempt_event_ordinal,
        scan_token,
        event,
    )
    .await?;
    Ok(seq)
}

/// The sequence the next event appended to this chat takes.
async fn next_event_seq<C>(conn: &C, chat_id: ChatId) -> Result<i64>
where
    C: ConnectionTrait,
{
    let last = entities::code_event::Entity::find()
        .filter(entities::code_event::Column::SessionId.eq(chat_id.0))
        .order_by_desc(entities::code_event::Column::Seq)
        .one(conn)
        .await
        .map_err(store_err)?;
    last.map_or(Some(1), |model| model.seq.checked_add(1))
        .ok_or_else(|| AgentError::Store(format!("event sequence exhausted for chat {chat_id}")))
}

/// Write one chat event as a row of the session's journal.
///
/// The chat lane and the code session worker append to the same table under
/// the same session row lock, so their sequences interleave without gaps or
/// collisions. The row carries the code vocabulary (`crate::chat_journal`)
/// and the lane's recovery receipts beside it.
#[allow(clippy::too_many_arguments)]
async fn insert_event_row<C>(
    conn: &C,
    chat_id: ChatId,
    seq: i64,
    turn_id: Option<TurnId>,
    lease_token: Option<uuid::Uuid>,
    attempt_event_ordinal: Option<i32>,
    scan_token: Option<uuid::Uuid>,
    event: &AgentEvent,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let owner = entities::code_session::Entity::find_by_id(chat_id.0)
        .select_only()
        .column(entities::code_session::Column::Owner)
        .into_tuple::<String>()
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("chat {chat_id} does not exist")))?;
    entities::code_event::ActiveModel {
        session_id: Set(chat_id.0),
        seq: Set(seq),
        owner: Set(owner),
        event: Set(serde_json::to_value(crate::chat_journal::journal_row(
            event,
        ))?),
        created_at: Set(Utc::now()),
        turn_id: Set(turn_id.map(|id| id.0)),
        lease_token: Set(lease_token),
        attempt_event_ordinal: Set(attempt_event_ordinal),
        scan_token: Set(scan_token),
        terminal: Set(turn_id.is_some() && is_terminal_event(event)),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

fn is_terminal_event(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::TurnCompleted { .. }
            | AgentEvent::TurnRefused { .. }
            | AgentEvent::TurnFailed { .. }
            | AgentEvent::TurnCancelled { .. }
    )
}

pub(in crate::db) async fn list_events(
    store: &DbStore,
    chat_id: ChatId,
    after: i64,
) -> Result<Vec<SequencedEvent>> {
    decode_sequenced_events(
        entities::code_event::Entity::find()
            .filter(entities::code_event::Column::SessionId.eq(chat_id.0))
            .filter(entities::code_event::Column::Seq.gt(after))
            .order_by_asc(entities::code_event::Column::Seq)
            .all(&store.conn)
            .await
            .map_err(store_err)?,
    )
}

/// Journal rows for one tool call: args deltas and completions, in seq order.
///
/// The payload JSON is the only place `call_id` lives, so this filters
/// server-side the same way terminal-turn snapshots already filter on
/// `type`. Call ids are UUIDs; interpolating the hyphenated form is safe
/// and avoids a `?` placeholder inside a JSON expression (Postgres treats
/// `?` as a jsonb operator).
pub(in crate::db) async fn list_events_for_call(
    store: &DbStore,
    chat_id: ChatId,
    call_id: CallId,
) -> Result<Vec<SequencedEvent>> {
    let call_id = call_id.0;
    let matches = match store.conn.get_database_backend() {
        DatabaseBackend::Postgres => format!(
            "event ->> 'type' IN ('{TOOL_CALL_ARGS_DELTA_TAG}', '{TOOL_CALL_COMPLETED_TAG}') \
             AND event ->> 'call_id' = '{call_id}'"
        ),
        _ => format!(
            "json_extract(event, '$.type') IN ('{TOOL_CALL_ARGS_DELTA_TAG}', '{TOOL_CALL_COMPLETED_TAG}') \
             AND json_extract(event, '$.call_id') = '{call_id}'"
        ),
    };
    decode_sequenced_events(
        entities::code_event::Entity::find()
            .filter(entities::code_event::Column::SessionId.eq(chat_id.0))
            .filter(sea_orm::sea_query::Expr::cust(matches))
            .order_by_asc(entities::code_event::Column::Seq)
            .all(&store.conn)
            .await
            .map_err(store_err)?,
    )
}

/// The chat reading of journal rows. Rows only an external engine writes
/// have none and are skipped, so a chat replay may show gaps in `seq`; the
/// cursor contract only needs the numbers to ascend.
fn decode_sequenced_events(rows: Vec<entities::code_event::Model>) -> Result<Vec<SequencedEvent>> {
    rows.into_iter()
        .filter_map(|model| {
            crate::chat_journal::decode_chat_event(model.event)
                .transpose()
                .map(|event| {
                    Ok(SequencedEvent {
                        seq: model.seq,
                        event: event?,
                    })
                })
        })
        .collect()
}

fn chat_from_models(
    model: entities::code_session::Model,
    rows: Vec<entities::chat_root_attachment::Model>,
) -> Result<Chat> {
    if rows.len() > MAX_ROOT_ATTACHMENTS {
        return Err(AgentError::Store(format!(
            "chat {} exceeds the root attachment limit",
            model.id
        )));
    }
    let root_attachments = rows
        .into_iter()
        .enumerate()
        .map(|(expected, row)| {
            if usize::try_from(row.position).ok() != Some(expected) {
                return Err(AgentError::Store(format!(
                    "chat {} root positions are not contiguous",
                    model.id
                )));
            }
            Ok(ChatRootAttachment {
                root_id: HostRootId::from_uuid(row.root_id).map_err(|error| {
                    AgentError::Store(format!("chat {} has an invalid root id: {error}", model.id))
                })?,
                origin: attachment_origin_from_db(&row.origin)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let chat = Chat {
        id: ChatId(model.id),
        project_id: model.project_id.map(ProjectId),
        title: model.title,
        model: model.model,
        reasoning_effort: model
            .reasoning_effort
            .as_deref()
            .and_then(ReasoningEffort::from_str),
        permission_mode: model
            .permission_mode
            .as_deref()
            .and_then(PermissionMode::from_str),
        network_policy: serde_json::from_str(&model.network_policy).map_err(|error| {
            AgentError::Store(format!(
                "chat {} has an invalid network policy: {error}",
                model.id
            ))
        })?,
        attachment_revision: model.attachment_revision,
        root_attachments,
        memory_incognito: model.memory_incognito,
        created_at: model.created_at,
    };
    validate_chat_root_projection(&chat).map_err(|message| AgentError::Store(message.into()))?;
    Ok(chat)
}

fn validate_chat_attachments(chat: &Chat, project_roots: &[HostRootId]) -> Result<()> {
    validate_chat_root_projection_against_project(chat, project_roots)
        .map_err(|message| AgentError::Store(message.into()))
}

pub(in crate::db) fn attachment_origin_to_db(origin: RootAttachmentOrigin) -> &'static str {
    match origin {
        RootAttachmentOrigin::ProjectDefault => "project_default",
        RootAttachmentOrigin::Conversation => "conversation",
    }
}

pub(in crate::db) fn attachment_origin_from_db(value: &str) -> Result<RootAttachmentOrigin> {
    match value {
        "project_default" => Ok(RootAttachmentOrigin::ProjectDefault),
        "conversation" => Ok(RootAttachmentOrigin::Conversation),
        other => Err(AgentError::Store(format!(
            "unknown chat root attachment origin: {other}"
        ))),
    }
}

fn message_from_model(model: entities::message::Model) -> Result<Message> {
    Ok(Message {
        id: MessageId(model.id),
        chat_id: ChatId(model.chat_id),
        turn_id: TurnId(model.turn_id),
        role: role_from_db(&model.role)?,
        content: model.content,
        llm_content: model.llm_content,
        reasoning: reasoning_from_db(model.reasoning),
        created_at: model.created_at,
    })
}

/// Reasoning as it is stored: absent when there is nothing to replay, so a
/// message without reasoning writes the same row it always did.
pub(in crate::db) fn reasoning_to_db(reasoning: &MessageReasoning) -> Option<serde_json::Value> {
    if reasoning.is_empty() {
        return None;
    }
    serde_json::to_value(reasoning).ok()
}

/// Reasoning as it is read back.
///
/// Replay is an optimization on top of the transcript, so a column this
/// version cannot parse degrades to no reasoning rather than failing the load
/// of an otherwise intact conversation.
pub(in crate::db) fn reasoning_from_db(stored: Option<serde_json::Value>) -> MessageReasoning {
    stored
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

/// `Role` is persisted as its snake_case name (matching its serde encoding).
fn role_to_db(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn role_from_db(text: &str) -> Result<Role> {
    match text {
        "system" => Ok(Role::System),
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        "tool" => Ok(Role::Tool),
        other => Err(AgentError::Store(format!("unknown role: {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::CallId;
    use crate::preview::{ResultEntry, ResultEntryKind, ToolResultPreview};

    fn terminal_call(name: &str, error_code: Option<&str>) -> ToolCallRecord {
        ToolCallRecord {
            id: CallId::new(),
            chat_id: ChatId::new(),
            turn_id: crate::TurnId::new(),
            provider_id: "provider".into(),
            name: name.into(),
            arguments: serde_json::json!({}),
            raw_arguments: None,
            execution: crate::model::ToolCallExecution::Server,
            status: crate::model::ToolCallStatus::Completed,
            result: Some("model-facing text".into()),
            result_preview: None,
            provider_replay: None,
            error_code: error_code.map(Into::into),
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: chrono::Utc::now(),
            resolved_at: Some(chrono::Utc::now()),
        }
    }

    /// The whole point of retaining a projection: reopening a chat shows the
    /// card the reader saw live, rather than the muted rail line that used to
    /// be all history could rebuild.
    #[test]
    fn a_retained_projection_comes_back_as_the_card_it_was() {
        let stored = serde_json::to_value(ToolResultPreview::Entries {
            entries: vec![ResultEntry::new(ResultEntryKind::File, "notes.md")],
            failures: Vec::new(),
            elided: 0,
        })
        .unwrap();
        let activity = tool_activity_from_call(terminal_call("list_dir", None), Some(stored));
        assert_eq!(
            activity.result,
            Some(ToolResultPreview::Entries {
                entries: vec![ResultEntry::new(ResultEntryKind::File, "notes.md")],
                failures: Vec::new(),
                elided: 0,
            })
        );
        assert!(!activity.result_unreadable);
    }

    #[test]
    fn browser_tool_activity_keeps_its_fixed_renderer_name() {
        for (name, expected) in [
            (
                crate::BROWSER_LIST_TOOL,
                crate::RendererToolName::BrowserList,
            ),
            (
                crate::BROWSER_NAVIGATE_TOOL,
                crate::RendererToolName::BrowserNavigate,
            ),
            (
                crate::BROWSER_SNAPSHOT_TOOL,
                crate::RendererToolName::BrowserSnapshot,
            ),
            (
                crate::BROWSER_WAIT_TOOL,
                crate::RendererToolName::BrowserWait,
            ),
            (
                crate::BROWSER_SCREENSHOT_TOOL,
                crate::RendererToolName::BrowserScreenshot,
            ),
            (crate::BROWSER_ACT_TOOL, crate::RendererToolName::BrowserAct),
        ] {
            let activity = tool_activity_from_call(terminal_call(name, None), None);
            assert_eq!(activity.tool, expected);
        }
    }

    /// The projection is a closed union that is allowed to move, so a row
    /// written by an older build may no longer parse. Rendering nothing would
    /// claim the call produced nothing, which is a different and untrue thing.
    #[test]
    fn a_projection_this_build_cannot_read_says_so_rather_than_vanishing() {
        let activity = tool_activity_from_call(
            terminal_call("list_dir", None),
            Some(serde_json::json!({ "tool": "a_variant_from_the_future" })),
        );
        assert_eq!(activity.result, None);
        assert!(activity.result_unreadable);
    }

    /// "You said no" and "the tool broke" are different facts to show a
    /// reader; folding a decline into `Failed` made history claim a crash.
    #[test]
    fn a_declined_call_projects_denied_while_other_failures_stay_failed() {
        let declined = ToolCallRecord {
            status: crate::model::ToolCallStatus::Failed,
            ..terminal_call("exec", Some("user_declined"))
        };
        assert_eq!(
            tool_activity_from_call(declined, None).status,
            ChatToolActivityStatus::Denied
        );
        let crashed = ToolCallRecord {
            status: crate::model::ToolCallStatus::Failed,
            ..terminal_call("exec", Some("tool_failed"))
        };
        assert_eq!(
            tool_activity_from_call(crashed, None).status,
            ChatToolActivityStatus::Failed
        );
    }

    /// Calls that resolved before projections were retained still rebuild the
    /// one enumerated signal history could always recover.
    ///
    #[test]
    fn a_call_with_no_retained_projection_falls_back_to_its_stored_signal() {
        let activity = tool_activity_from_call(
            terminal_call(crate::WEB_SEARCH_TOOL, Some("configuration_required")),
            None,
        );
        assert_eq!(
            activity.result,
            Some(ToolResultPreview::WebSearchProviderRequired)
        );
        assert!(!activity.result_unreadable);
    }
}
