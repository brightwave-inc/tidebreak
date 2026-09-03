use chrono::{DateTime, Utc};
use sea_orm::Set;
use uuid::Uuid;

use crate::error::{AgentError, Result};
use crate::id::{HostRootId, RootAttachmentChangeId, SessionId};
use crate::model::{
    RootAttachmentChange, RootAttachmentChangeAction, RootAttachmentChangeFailure,
    RootAttachmentChangePhase, RootAttachmentChangeTerminal, RootAttachmentSubjectKind,
};

use super::super::super::entities;
use super::super::conversation::{attachment_origin_from_db, attachment_origin_to_db};
use super::super::turn::canonical_db_timestamp;

pub(super) fn change_active_model(
    change: &RootAttachmentChange,
) -> entities::root_attachment_change::ActiveModel {
    entities::root_attachment_change::ActiveModel {
        id: Set(*change.id.as_uuid()),
        chat_id: Set(change.chat_id.0),
        subject_kind: Set(subject_kind_to_db(change.subject_kind).into()),
        subject_id: Set(change.subject_id),
        executor_id: Set(change.executor_id),
        root_id: Set(*change.root_id.as_uuid()),
        action: Set(action_to_db(change.action).into()),
        origin: Set(change
            .origin
            .map(|origin| attachment_origin_to_db(origin).into())),
        projection_position: Set(change
            .projection_position
            .map(i64::from)
            .map(|position| i32::try_from(position).expect("bounded projection position"))),
        projection_existed_before: Set(change.projection_existed_before),
        expected_revision: Set(change.expected_revision),
        before_revision: Set(change.before_revision),
        intent_revision: Set(change.intent_revision),
        phase: Set(phase_to_db(change.phase).into()),
        result_revision: Set(change.result_revision),
        projection_changed: Set(change.projection_changed),
        broker_changed: Set(change.broker_changed),
        broker_currently_attached: Set(change.broker_currently_attached),
        failure_code: Set(change.failure.as_ref().map(|failure| failure.code.clone())),
        failure_message: Set(change
            .failure
            .as_ref()
            .map(|failure| failure.message.clone())),
        failure_retryable: Set(change.failure.as_ref().map(|failure| failure.retryable)),
        created_at: Set(change.created_at),
        finished_at: Set(change.finished_at),
    }
}

pub(super) fn change_from_model(
    model: entities::root_attachment_change::Model,
) -> Result<RootAttachmentChange> {
    let created_at = require_canonical_timestamp("created_at", model.id, model.created_at)?;
    let finished_at = model
        .finished_at
        .map(|value| require_canonical_timestamp("finished_at", model.id, value))
        .transpose()?;
    let failure = match (
        model.failure_code,
        model.failure_message,
        model.failure_retryable,
    ) {
        (None, None, None) => None,
        (Some(code), Some(message), Some(retryable)) => Some(RootAttachmentChangeFailure {
            code,
            message,
            retryable,
        }),
        _ => {
            return Err(AgentError::Store(format!(
                "root attachment change {} has partial failure fields",
                model.id
            )))
        }
    };
    let change = RootAttachmentChange {
        id: RootAttachmentChangeId::from_uuid(model.id).map_err(|error| {
            AgentError::Store(format!("invalid root attachment change id: {error}"))
        })?,
        chat_id: SessionId(model.chat_id),
        executor_id: model.executor_id,
        root_id: HostRootId::from_uuid(model.root_id).map_err(|error| {
            AgentError::Store(format!(
                "root attachment change {} has invalid root id: {error}",
                model.id
            ))
        })?,
        action: action_from_db(&model.action)?,
        subject_kind: subject_kind_from_db(&model.subject_kind)?,
        subject_id: model.subject_id,
        origin: model
            .origin
            .as_deref()
            .map(attachment_origin_from_db)
            .transpose()?,
        projection_position: model
            .projection_position
            .map(|position| {
                u32::try_from(position).map_err(|_| {
                    AgentError::Store(format!(
                        "root attachment change {} has an invalid projection position",
                        model.id
                    ))
                })
            })
            .transpose()?,
        projection_existed_before: model.projection_existed_before,
        expected_revision: model.expected_revision,
        before_revision: model.before_revision,
        intent_revision: model.intent_revision,
        phase: phase_from_db(&model.phase)?,
        result_revision: model.result_revision,
        projection_changed: model.projection_changed,
        broker_changed: model.broker_changed,
        broker_currently_attached: model.broker_currently_attached,
        failure,
        created_at,
        finished_at,
    };
    change.validate().map_err(|message| {
        AgentError::Store(format!(
            "invalid root attachment change {}: {message}",
            change.id
        ))
    })?;
    Ok(change)
}

fn require_canonical_timestamp(
    field: &str,
    id: Uuid,
    value: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    let canonical = canonical_db_timestamp(value)?;
    if canonical != value {
        return Err(AgentError::Store(format!(
            "root attachment change {id} has noncanonical {field}"
        )));
    }
    Ok(canonical)
}

pub(super) fn terminal_matches(
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

fn action_to_db(action: RootAttachmentChangeAction) -> &'static str {
    match action {
        RootAttachmentChangeAction::Attach => "attach",
        RootAttachmentChangeAction::Detach => "detach",
    }
}

fn action_from_db(value: &str) -> Result<RootAttachmentChangeAction> {
    match value {
        "attach" => Ok(RootAttachmentChangeAction::Attach),
        "detach" => Ok(RootAttachmentChangeAction::Detach),
        other => Err(AgentError::Store(format!(
            "unknown root attachment action: {other}"
        ))),
    }
}

fn subject_kind_to_db(kind: RootAttachmentSubjectKind) -> &'static str {
    match kind {
        RootAttachmentSubjectKind::Project => "project",
        RootAttachmentSubjectKind::Conversation => "conversation",
    }
}

fn subject_kind_from_db(value: &str) -> Result<RootAttachmentSubjectKind> {
    match value {
        "project" => Ok(RootAttachmentSubjectKind::Project),
        "conversation" => Ok(RootAttachmentSubjectKind::Conversation),
        other => Err(AgentError::Store(format!(
            "unknown root attachment subject kind: {other}"
        ))),
    }
}

pub(super) fn phase_to_db(phase: RootAttachmentChangePhase) -> &'static str {
    match phase {
        RootAttachmentChangePhase::AwaitingBroker => "awaiting_broker",
        RootAttachmentChangePhase::Completed => "completed",
        RootAttachmentChangePhase::Failed => "failed",
    }
}

fn phase_from_db(value: &str) -> Result<RootAttachmentChangePhase> {
    match value {
        "awaiting_broker" => Ok(RootAttachmentChangePhase::AwaitingBroker),
        "completed" => Ok(RootAttachmentChangePhase::Completed),
        "failed" => Ok(RootAttachmentChangePhase::Failed),
        other => Err(AgentError::Store(format!(
            "unknown root attachment change phase: {other}"
        ))),
    }
}
