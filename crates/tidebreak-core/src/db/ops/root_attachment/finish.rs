use chrono::{DateTime, Utc};
use sea_orm::{EntityTrait, TransactionTrait};
use uuid::Uuid;

use crate::error::{AgentError, Result};
use crate::id::RootAttachmentChangeId;
use crate::model::{
    RootAttachmentChangeAction, RootAttachmentChangePhase, RootAttachmentChangeTerminal,
};
use crate::storage::FinishRootAttachmentChangeOutcome;

use super::super::super::{entities, store_err, DbStore};
use super::super::acquire_chat_write_lock;
use super::super::turn::canonical_db_timestamp;
use super::codec::terminal_matches;
use super::persistence::{find_change, find_change_on, persist_terminal_change};
use super::projection::{
    desired_state, load_projection, remove_projection_row, validate_pending_projection,
};

pub(in crate::db) async fn finish_root_attachment_change(
    store: &DbStore,
    id: RootAttachmentChangeId,
    executor_id: Uuid,
    terminal: &RootAttachmentChangeTerminal,
    finished_at: DateTime<Utc>,
) -> Result<FinishRootAttachmentChangeOutcome> {
    if executor_id.is_nil() {
        return Err(AgentError::Store(
            "root attachment change executor id must not be nil".into(),
        ));
    }
    terminal
        .validate()
        .map_err(|message| AgentError::Store(message.into()))?;
    let finished_at = canonical_db_timestamp(finished_at)?;
    let Some(initial) = find_change(store, id).await? else {
        return Ok(FinishRootAttachmentChangeOutcome::NotFound);
    };
    if initial.executor_id != executor_id {
        return Ok(FinishRootAttachmentChangeOutcome::ExecutorMismatch);
    }

    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, initial.chat_id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "root attachment change {id} references missing chat {}",
            initial.chat_id
        )));
    }
    let Some(mut change) = find_change_on(&transaction, id).await? else {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "root attachment change {id} disappeared while locked"
        )));
    };
    if change.executor_id != executor_id {
        transaction.commit().await.map_err(store_err)?;
        return Ok(FinishRootAttachmentChangeOutcome::ExecutorMismatch);
    }
    if change.phase != RootAttachmentChangePhase::AwaitingBroker {
        let outcome = if terminal_matches(&change, terminal) {
            FinishRootAttachmentChangeOutcome::Existing(change)
        } else {
            FinishRootAttachmentChangeOutcome::AlreadyTerminal(change)
        };
        transaction.commit().await.map_err(store_err)?;
        return Ok(outcome);
    }
    // Creation time is immutable caller identity, while finish time is
    // server-owned metadata. Clamp under the operation lock so clock skew or a
    // wall-clock rollback cannot wedge this chat's single pending slot.
    let finished_at = finished_at.max(change.created_at);

    if let RootAttachmentChangeTerminal::Completed {
        broker_currently_attached,
        ..
    } = terminal
    {
        let desired = change.action == RootAttachmentChangeAction::Attach;
        if *broker_currently_attached != desired {
            transaction.commit().await.map_err(store_err)?;
            return Ok(FinishRootAttachmentChangeOutcome::BrokerStateMismatch);
        }
    }
    if let RootAttachmentChangeTerminal::Failed {
        broker_currently_attached: Some(broker_currently_attached),
        ..
    } = terminal
    {
        let desired = change.action == RootAttachmentChangeAction::Attach;
        if *broker_currently_attached == desired {
            transaction.commit().await.map_err(store_err)?;
            return Ok(FinishRootAttachmentChangeOutcome::BrokerStateMismatch);
        }
    }

    let chat = entities::chat::Entity::find_by_id(change.chat_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!("chat {} disappeared while locked", change.chat_id))
        })?;
    if chat.attachment_revision != change.intent_revision {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "chat {} attachment revision changed while root attachment change {id} was pending",
            change.chat_id
        )));
    }
    let projection =
        load_projection(&transaction, change.chat_id, chat.attachment_revision).await?;
    validate_pending_projection(&change, &projection)?;

    let completed = matches!(terminal, RootAttachmentChangeTerminal::Completed { .. });
    let remove_projection = (completed
        && change.action == RootAttachmentChangeAction::Detach
        && change.projection_existed_before)
        || (!completed
            && change.action == RootAttachmentChangeAction::Attach
            && !change.projection_existed_before);
    let result_revision = if remove_projection {
        let position = change.projection_position.ok_or_else(|| {
            AgentError::Store(format!(
                "root attachment change {id} has no projection position"
            ))
        })?;
        remove_projection_row(
            &transaction,
            change.chat_id,
            change.root_id,
            position,
            change.intent_revision,
        )
        .await?;
        change.intent_revision.checked_add(1).ok_or_else(|| {
            AgentError::Store(format!("root attachment change {id} exhausted revisions"))
        })?
    } else {
        change.intent_revision
    };

    change.phase = if completed {
        RootAttachmentChangePhase::Completed
    } else {
        RootAttachmentChangePhase::Failed
    };
    change.result_revision = Some(result_revision);
    change.projection_changed =
        Some(completed && change.projection_existed_before != desired_state(&change));
    match terminal {
        RootAttachmentChangeTerminal::Completed {
            broker_changed,
            broker_currently_attached,
        } => {
            change.broker_changed = Some(*broker_changed);
            change.broker_currently_attached = Some(*broker_currently_attached);
            change.failure = None;
        }
        RootAttachmentChangeTerminal::Failed {
            broker_changed,
            broker_currently_attached,
            failure,
        } => {
            change.broker_changed = *broker_changed;
            change.broker_currently_attached = *broker_currently_attached;
            change.failure = Some(failure.clone());
        }
    }
    change.finished_at = Some(finished_at);
    change
        .validate()
        .map_err(|message| AgentError::Store(message.into()))?;
    persist_terminal_change(&transaction, &change).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(FinishRootAttachmentChangeOutcome::Finished(change))
}
