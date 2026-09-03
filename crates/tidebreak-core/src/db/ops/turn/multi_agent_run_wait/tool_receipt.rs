use sea_orm::{ConnectionTrait, EntityTrait};

use crate::agent_tools::{
    canonical_wait_for_agents_result, WAIT_CANCELLED_WITH_TURN_RESULT, WAIT_FOR_AGENTS_TOOL,
    WAIT_INTERRUPTED_BY_STEER_RESULT,
};
use crate::error::Result;
use crate::event::AgentEvent;
use crate::model::{
    AgentRunInboxStatus, AgentRunWaitSetCheckpointRequest, ToolCallExecution, ToolCallStatus,
    TurnAgentRunWaitStatus,
};
use crate::{AgentRunId, CallId};

use super::{entities, load_agent_run_inbox_by_ids_on, load_members, store_err};

pub(super) fn exact_wait_call_request(
    call: &entities::tool_call::Model,
    wait: &entities::turn_agent_run_wait_set::Model,
    request: &AgentRunWaitSetCheckpointRequest,
) -> bool {
    call.id == wait.id
        && call.chat_id == wait.session_id
        && call.turn_id == wait.turn_id
        && call.provider_id == request.provider_id
        && call.name == WAIT_FOR_AGENTS_TOOL
        && call.arguments == request.arguments
        && call.execution == ToolCallExecution::Orchestration.as_str()
        && call.error_code.is_none()
        && call.error_detail.is_none()
        && orchestration_call_has_no_auxiliary_state(call)
}

pub(super) async fn exact_wait_lifecycle_on<C>(
    conn: &C,
    call: &entities::tool_call::Model,
    wait: &entities::turn_agent_run_wait_set::Model,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    if wait.status == TurnAgentRunWaitStatus::Waiting.as_str() {
        return Ok(exact_pending_wait_call_model(call, wait)
            && wait.closed_at.is_none()
            && wait.resume_token.is_none()
            && wait.event_seq.is_none());
    }
    let expected_status = if wait.status == TurnAgentRunWaitStatus::Resumed.as_str() {
        let Some(resume_token) = wait.resume_token else {
            return Ok(false);
        };
        let members = load_members(conn, CallId(wait.id)).await?;
        let mut entries = Vec::with_capacity(members.len());
        for member in members {
            let Some(entry) = load_agent_run_inbox_by_ids_on(
                conn,
                AgentRunId(wait.parent_run_id),
                AgentRunId(member.child_run_id),
            )
            .await?
            else {
                return Ok(false);
            };
            if entry.status != AgentRunInboxStatus::Consumed
                || entry.consumed_lease_token != Some(resume_token)
            {
                return Ok(false);
            }
            entries.push(entry);
        }
        if call.result.as_deref() != Some(canonical_wait_for_agents_result(&entries)?.as_str()) {
            return Ok(false);
        }
        ToolCallStatus::Completed
    } else if wait.status == TurnAgentRunWaitStatus::Cancelled.as_str() {
        if !matches!(
            call.result.as_deref(),
            Some(WAIT_INTERRUPTED_BY_STEER_RESULT | WAIT_CANCELLED_WITH_TURN_RESULT)
        ) {
            return Ok(false);
        }
        ToolCallStatus::Cancelled
    } else {
        return Ok(false);
    };
    let Some(result) = call.result.as_deref() else {
        return Ok(false);
    };
    if !exact_terminal_wait_call(call, wait, expected_status, result) {
        return Ok(false);
    }
    let Some(event_seq) = wait.event_seq else {
        return Ok(false);
    };
    let Some(event) = entities::event::Entity::find_by_id((wait.session_id, event_seq))
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(false);
    };
    let expected = AgentEvent::ToolCallCompleted {
        call_id: CallId(wait.id),
        output: if call.status == ToolCallStatus::Cancelled.as_str() {
            crate::ToolOutput::error(result)
        } else {
            crate::ToolOutput::text(result)
        },
        action: None,
        result: None,
    };
    Ok(event.turn_id == Some(wait.turn_id)
        && event.lease_token == Some(wait.park_lease_token)
        && event.attempt_event_ordinal == Some(wait.event_ordinal)
        && !event.terminal
        && crate::chat_journal::decode_chat_event_required(event.event)? == expected)
}

pub(super) fn exact_pending_wait_call_model(
    call: &entities::tool_call::Model,
    wait: &entities::turn_agent_run_wait_set::Model,
) -> bool {
    call.id == wait.id
        && call.chat_id == wait.session_id
        && call.turn_id == wait.turn_id
        && call.name == WAIT_FOR_AGENTS_TOOL
        && call.execution == ToolCallExecution::Orchestration.as_str()
        && call.status == ToolCallStatus::Pending.as_str()
        && call.created_at == wait.parked_at
        && call.result.is_none()
        && call.error_code.is_none()
        && call.error_detail.is_none()
        && call.resolved_at.is_none()
        && orchestration_call_has_no_auxiliary_state(call)
}

pub(super) fn exact_terminal_wait_call(
    call: &entities::tool_call::Model,
    wait: &entities::turn_agent_run_wait_set::Model,
    status: ToolCallStatus,
    result: &str,
) -> bool {
    call.id == wait.id
        && call.chat_id == wait.session_id
        && call.turn_id == wait.turn_id
        && call.name == WAIT_FOR_AGENTS_TOOL
        && call.execution == ToolCallExecution::Orchestration.as_str()
        && call.status == status.as_str()
        && call.created_at == wait.parked_at
        && call.result.as_deref() == Some(result)
        && call.error_code.is_none()
        && call.error_detail.is_none()
        && call.resolved_at == wait.closed_at
        && orchestration_call_has_no_auxiliary_state(call)
}

fn orchestration_call_has_no_auxiliary_state(call: &entities::tool_call::Model) -> bool {
    call.client_executor_id.is_none()
        && call.client_lease_token.is_none()
        && call.client_lease_expires_at.is_none()
}
