//! Durable conversation-tool request jobs for the channel adapter.
//!
//! A native Slack tool writes the exact operation and arguments when it
//! needs Slack payload provenance; the adapter lists pending jobs under its
//! live grant and posts an untrusted result back. Content is stored as
//! bounded JSON text and never names a channel, repository, or path the
//! storage layer trusts; every read still authorizes through the stored
//! owner, session, grant, and binding.

use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
    TransactionTrait,
};
use serde_json::Value;

use base64::Engine as _;

use crate::code::{CodeBindingId, CodeGrantId, ConversationRequest, SessionId};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};
use super::super::agent_run::database_now;

fn request_from_model(
    model: entities::code_conversation_request::Model,
) -> Result<ConversationRequest> {
    Ok(ConversationRequest {
        id: model.id,
        binding_id: CodeBindingId(model.binding_id),
        operation: model.operation,
        arguments: model.arguments,
        result: model.result,
    })
}

/// A stored request answers as absent unless its owner, session, grant, and
/// binding are all currently live. `session` rows name fenced and ended
/// states; a revoked grant row likewise stops matching.
async fn require_live_scope<C>(
    connection: &C,
    owner: &OwnerId,
    session_id: SessionId,
    grant_id: CodeGrantId,
    binding_id: CodeBindingId,
) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    let grant = entities::code_external_grant::Entity::find_by_id(grant_id.0)
        .filter(entities::code_external_grant::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_external_grant::Column::RevokedAt.is_null())
        .one(connection)
        .await
        .map_err(store_err)?;
    if grant.is_none() {
        return Err(AgentError::InvalidTarget(
            "the adapter grant is no longer live".into(),
        ));
    }
    let session = entities::session::Entity::find_by_id(session_id.0)
        .filter(entities::session::Column::Owner.eq(owner.as_str()))
        .one(connection)
        .await
        .map_err(store_err)?;
    if session.is_none_or(|row| {
        row.lifecycle == crate::code::SessionLifecycle::Ended.as_str()
            || row.lifecycle == crate::code::SessionLifecycle::Fenced.as_str()
    }) {
        return Err(AgentError::InvalidTarget(
            "the bound session is no longer live".into(),
        ));
    }
    let binding = entities::code_external_binding::Entity::find_by_id(binding_id.0)
        .filter(entities::code_external_binding::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_external_binding::Column::SessionId.eq(session_id.0))
        .filter(entities::code_external_binding::Column::GrantId.eq(grant_id.0))
        .one(connection)
        .await
        .map_err(store_err)?;
    if binding.is_none() {
        return Err(AgentError::InvalidTarget(
            "the conversation binding is no longer live".into(),
        ));
    }
    Ok(())
}

/// Reject malformed or oversized JSON before any allocation reaches the
/// database. Every request and result is untrusted adapter/model data and
/// stays bounded at 3 MiB of canonical serialized bytes.
fn validate_bounded_json(kind: &str, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(AgentError::Serde)?;
    if bytes.len() > ConversationRequest::MAX_JSON_BYTES {
        return Err(AgentError::InvalidTarget(format!(
            "{kind} exceeds the {} MiB conversation request cap",
            ConversationRequest::MAX_JSON_BYTES / (1024 * 1024)
        )));
    }
    Ok(())
}

/// Require small bounded field values without ever treating message text as
/// instructions, permissions, or authorization.
fn bounded_string<'a>(value: &'a Value, max: usize, kind: &str) -> Result<&'a str> {
    let text = value
        .as_str()
        .ok_or_else(|| AgentError::InvalidTarget(format!("{kind} must be a string")))?;
    if text.len() > max {
        return Err(AgentError::InvalidTarget(format!(
            "{kind} exceeds {max} bytes"
        )));
    }
    Ok(text)
}

fn bounded_optional_string(value: Option<&Value>, max: usize, kind: &str) -> Result<()> {
    if let Some(value) = value.filter(|v| !v.is_null()) {
        bounded_string(value, max, kind)?;
    }
    Ok(())
}

/// The caller's budget includes the whole canonical JSON envelope, including
/// metadata, errors, and fields that the reader does not recognize.
fn validate_result_budget(operation: &str, arguments: &Value, result: &Value) -> Result<()> {
    let (default_bytes, hard_bytes) = match operation {
        "read" => (16_384, ConversationRequest::READ_MAX_BYTES),
        "export" => (1_048_576, ConversationRequest::EXPORT_MAX_BYTES),
        "attachment" if result.get("error").is_some() => (8_192, 8_192),
        "attachment" => (
            ConversationRequest::MAX_JSON_BYTES,
            ConversationRequest::MAX_JSON_BYTES,
        ),
        _ => {
            return Err(AgentError::InvalidTarget(
                "unknown conversation operation".into(),
            ))
        }
    };
    let requested = if operation == "attachment" {
        default_bytes as u64
    } else {
        arguments
            .get("max_bytes")
            .and_then(Value::as_u64)
            .unwrap_or(default_bytes as u64)
    };
    let cap = requested.min(hard_bytes as u64);
    let bytes = serde_json::to_vec(result).map_err(AgentError::Serde)?.len();
    if bytes as u64 > cap {
        return Err(AgentError::InvalidTarget(format!(
            "result totals {bytes} serialized bytes; the request allows at most {cap}"
        )));
    }
    Ok(())
}

/// Validate a completed `read`/`export` result against the original request
/// limits so a hostile or buggy adapter result cannot blow a model's context.
/// Counts and text honor the requested bounds as well as the hard caps.
fn validate_export_result(
    result: &Value,
    request_arguments: &Value,
    operation: &str,
) -> Result<()> {
    let messages = result
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| AgentError::InvalidTarget("result messages must be an array".into()))?;
    let (hard_messages, hard_text, requested_messages) = if operation == "read" {
        let requested = request_arguments
            .get("limit")
            .or_else(|| request_arguments.get("count"))
            .or_else(|| request_arguments.get("max_messages"))
            .and_then(Value::as_u64)
            .unwrap_or(ConversationRequest::READ_MAX_MESSAGES as u64);
        (
            ConversationRequest::READ_MAX_MESSAGES as u64,
            ConversationRequest::READ_MAX_BYTES as u64,
            requested,
        )
    } else {
        let requested = request_arguments
            .get("max_messages")
            .or_else(|| request_arguments.get("limit"))
            .or_else(|| request_arguments.get("count"))
            .and_then(Value::as_u64)
            .unwrap_or(ConversationRequest::EXPORT_MAX_MESSAGES as u64);
        (
            ConversationRequest::EXPORT_MAX_MESSAGES as u64,
            ConversationRequest::EXPORT_MAX_BYTES as u64,
            requested,
        )
    };
    let message_cap = requested_messages.min(hard_messages);
    if messages.len() as u64 > message_cap {
        return Err(AgentError::InvalidTarget(format!(
            "result has {} messages; the request allows at most {message_cap}",
            messages.len()
        )));
    }
    // Individual text fields also honor the operation's hard limit. The
    // full serialized envelope has already passed the request byte budget.
    let requested_text = request_arguments
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(hard_text);
    let text_cap = requested_text.min(hard_text);
    let mut total_text = 0_u64;
    for message in messages {
        bounded_string(
            message
                .get("id")
                .ok_or_else(|| AgentError::InvalidTarget("result message id is required".into()))?,
            512,
            "result message id",
        )?;
        bounded_string(
            message.get("timestamp").ok_or_else(|| {
                AgentError::InvalidTarget("result message timestamp is required".into())
            })?,
            128,
            "result message timestamp",
        )?;
        let author = message
            .get("author")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                AgentError::InvalidTarget("result message author must be an object".into())
            })?;
        bounded_string(
            author
                .get("id")
                .ok_or_else(|| AgentError::InvalidTarget("result author id is required".into()))?,
            512,
            "result author id",
        )?;
        bounded_string(
            author.get("name").ok_or_else(|| {
                AgentError::InvalidTarget("result author name is required".into())
            })?,
            512,
            "result author name",
        )?;
        bounded_string(
            author.get("kind").ok_or_else(|| {
                AgentError::InvalidTarget("result author kind is required".into())
            })?,
            64,
            "result author kind",
        )?;
        let text = bounded_string(
            message.get("text").ok_or_else(|| {
                AgentError::InvalidTarget("result message text is required".into())
            })?,
            usize::try_from(text_cap).unwrap_or(usize::MAX),
            "result message text",
        )?;
        total_text = total_text.saturating_add(text.len() as u64);
        bounded_optional_string(message.get("permalink"), 4_096, "result message permalink")?;
        let attachments: &[Value] = match message.get("attachments") {
            Some(Value::Array(values)) => values,
            None | Some(Value::Null) => &[],
            Some(_) => {
                return Err(AgentError::InvalidTarget(
                    "result message attachments must be an array".into(),
                ));
            }
        };
        if attachments.len() > 20 {
            return Err(AgentError::InvalidTarget(
                "result message has too many attachments".into(),
            ));
        }
        for attachment in attachments {
            bounded_string(
                attachment.get("id").ok_or_else(|| {
                    AgentError::InvalidTarget("result attachment id is required".into())
                })?,
                512,
                "result attachment id",
            )?;
            bounded_string(
                attachment.get("name").ok_or_else(|| {
                    AgentError::InvalidTarget("result attachment name is required".into())
                })?,
                1_024,
                "result attachment name",
            )?;
            bounded_optional_string(
                attachment.get("mime_type"),
                256,
                "result attachment mime_type",
            )?;
            bounded_optional_string(attachment.get("kind"), 128, "result attachment kind")?;
            bounded_optional_string(
                attachment.get("permalink"),
                4_096,
                "result attachment permalink",
            )?;
            if let Some(size) = attachment.get("size") {
                if !size.is_u64() {
                    return Err(AgentError::InvalidTarget(
                        "result attachment size must be a nonnegative integer".into(),
                    ));
                }
            }
            if attachment
                .get("readable")
                .and_then(Value::as_bool)
                .is_none()
            {
                return Err(AgentError::InvalidTarget(
                    "result attachment readable must be a boolean".into(),
                ));
            }
        }
    }
    if total_text > text_cap {
        return Err(AgentError::InvalidTarget(format!(
            "result message text totals {total_text} bytes; the request allows at most {text_cap}"
        )));
    }
    if result.get("source").and_then(Value::as_str) != Some("slack") {
        return Err(AgentError::InvalidTarget(
            "result source must be `slack`".into(),
        ));
    }
    bounded_optional_string(result.get("next_cursor"), 4_096, "result next_cursor")?;
    for flag in ["has_more", "truncated"] {
        if result.get(flag).and_then(Value::as_bool).is_none() {
            return Err(AgentError::InvalidTarget(format!(
                "result {flag} must be a boolean"
            )));
        }
    }
    Ok(())
}

/// Validate the three result envelopes against the request's operation and
/// requested bounds. The error envelope is small; attachment decodes before
/// storage so a >2 MiB payload is rejected rather than persisted.
fn validate_operation_result(operation: &str, arguments: &Value, result: &Value) -> Result<()> {
    validate_result_budget(operation, arguments, result)?;
    if result.get("error").is_some() {
        let error = result
            .get("error")
            .and_then(Value::as_object)
            .ok_or_else(|| AgentError::InvalidTarget("result error must be an object".into()))?;
        bounded_string(
            error
                .get("code")
                .ok_or_else(|| AgentError::InvalidTarget("result error code is required".into()))?,
            256,
            "result error code",
        )?;
        bounded_string(
            error.get("message").ok_or_else(|| {
                AgentError::InvalidTarget("result error message is required".into())
            })?,
            4_096,
            "result error message",
        )?;
        if let Some(retry) = error.get("retry_after_seconds") {
            if !retry.is_u64() {
                return Err(AgentError::InvalidTarget(
                    "result error retry_after_seconds must be a nonnegative integer".into(),
                ));
            }
        }
        return Ok(());
    }
    match operation {
        "read" | "export" => validate_export_result(result, arguments, operation),
        "attachment" => {
            let attachment = result
                .get("attachment")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    AgentError::InvalidTarget("result attachment must be an object".into())
                })?;
            bounded_string(
                attachment.get("id").ok_or_else(|| {
                    AgentError::InvalidTarget("result attachment id is required".into())
                })?,
                512,
                "result attachment id",
            )?;
            bounded_string(
                attachment.get("name").ok_or_else(|| {
                    AgentError::InvalidTarget("result attachment name is required".into())
                })?,
                1_024,
                "result attachment name",
            )?;
            bounded_string(
                attachment.get("mime_type").ok_or_else(|| {
                    AgentError::InvalidTarget("result attachment mime_type is required".into())
                })?,
                256,
                "result attachment mime_type",
            )?;
            bounded_string(
                attachment.get("kind").ok_or_else(|| {
                    AgentError::InvalidTarget("result attachment kind is required".into())
                })?,
                128,
                "result attachment kind",
            )?;
            if arguments
                .get("attachment_id")
                .is_some_and(|expected| attachment.get("id") != Some(expected))
            {
                return Err(AgentError::InvalidTarget(
                    "result attachment does not match the requested file".into(),
                ));
            }
            let data = bounded_string(
                result.get("data_base64").ok_or_else(|| {
                    AgentError::InvalidTarget("result attachment data_base64 is required".into())
                })?,
                ConversationRequest::MAX_JSON_BYTES,
                "result attachment data_base64",
            )?;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|_| {
                    AgentError::InvalidTarget("result attachment data_base64 is invalid".into())
                })?;
            if decoded.len() > ConversationRequest::ATTACHMENT_MAX_DECODED_BYTES {
                return Err(AgentError::InvalidTarget(format!(
                    "result attachment decodes to {} bytes; the cap is {}",
                    decoded.len(),
                    ConversationRequest::ATTACHMENT_MAX_DECODED_BYTES
                )));
            }
            Ok(())
        }
        other => Err(AgentError::InvalidTarget(format!(
            "unknown request operation `{other}`"
        ))),
    }
}

/// Insert one tool-call request, idempotent per `call_key`.
///
/// The unique key is scoped to one `(owner, session, grant, binding)`, so a
/// reconnect or retry with the same call key answers the same row. A replay
/// naming different operation or arguments is refused rather than silently
/// overwritten.
#[allow(clippy::too_many_arguments)]
pub async fn create_conversation_request(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    grant: CodeGrantId,
    binding: CodeBindingId,
    operation: &str,
    arguments: &Value,
    call_key: &str,
) -> Result<ConversationRequest> {
    if !ConversationRequest::OPERATIONS.contains(&operation) {
        return Err(AgentError::InvalidTarget(format!(
            "conversation request operation `{operation}` is not supported"
        )));
    }
    if call_key.trim().is_empty() || call_key.len() > 512 {
        return Err(AgentError::InvalidTarget(
            "a call key needs 1 to 512 bytes".into(),
        ));
    }
    validate_bounded_json("conversation request arguments", arguments)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    // Take the write lock before reading. This serializes same-call creates
    // on SQLite and PostgreSQL without a select-then-insert uniqueness race.
    if !super::acquire_code_session_write_lock(&transaction, session).await? {
        return Err(AgentError::InvalidTarget(
            "the bound session is no longer live".into(),
        ));
    }
    require_live_scope(&transaction, owner, session, grant, binding).await?;
    let existing = entities::code_conversation_request::Entity::find()
        .filter(entities::code_conversation_request::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_conversation_request::Column::SessionId.eq(session.0))
        .filter(entities::code_conversation_request::Column::GrantId.eq(grant.0))
        .filter(entities::code_conversation_request::Column::BindingId.eq(binding.0))
        .filter(entities::code_conversation_request::Column::CallKey.eq(call_key))
        .one(&transaction)
        .await
        .map_err(store_err)?;
    if let Some(row) = existing {
        let now = database_now(&transaction).await?;
        if now - row.created_at >= ConversationRequest::TTL {
            return Err(AgentError::InvalidTarget(
                "the conversation request has expired".into(),
            ));
        }
        let stored: Value = row.arguments.clone();
        if row.operation != operation || stored != *arguments {
            transaction.commit().await.map_err(store_err)?;
            return Err(AgentError::InvalidTarget(
                "a request already exists for this tool call with different operation or arguments"
                    .into(),
            ));
        }
        transaction.commit().await.map_err(store_err)?;
        return request_from_model(row);
    }
    let now = database_now(&transaction).await?;
    let id = uuid::Uuid::new_v4();
    let row = entities::code_conversation_request::ActiveModel {
        id: Set(id),
        owner: Set(owner.as_str().to_owned()),
        session_id: Set(session.0),
        grant_id: Set(grant.0),
        binding_id: Set(binding.0),
        call_key: Set(call_key.to_owned()),
        operation: Set(operation.to_owned()),
        arguments: Set(serde_json::to_value(arguments).map_err(AgentError::Serde)?),
        result: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    request_from_model(row)
}

/// Read one exact request back under the caller's live scope.
///
/// A request another grant or session owns answers `None`, matching the
/// adapter surface's not-found shape.
pub async fn get_conversation_request(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    grant: CodeGrantId,
    request_id: uuid::Uuid,
) -> Result<Option<ConversationRequest>> {
    let Some(row) = entities::code_conversation_request::Entity::find_by_id(request_id)
        .filter(entities::code_conversation_request::Column::Owner.eq(owner.as_str()))
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    if row.session_id != session.0 || row.grant_id != grant.0 {
        return Ok(None);
    }
    let now = database_now(&store.conn).await?;
    if now - row.created_at >= ConversationRequest::TTL {
        return Ok(None);
    }
    require_live_scope(
        &store.conn,
        owner,
        session,
        grant,
        CodeBindingId(row.binding_id),
    )
    .await?;
    Ok(Some(request_from_model(row)?))
}

/// List unexpired pending requests for one live session under one grant,
/// oldest-to-newest, at most [`ConversationRequest::MAX_PENDING_PER_SESSION`].
///
/// Pending polling never touches turn or message rows, so it cannot mutate
/// conversations or expose arbitrary histories.
pub async fn list_pending_conversation_requests(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    grant: CodeGrantId,
) -> Result<Vec<ConversationRequest>> {
    let bindings = entities::code_external_binding::Entity::find()
        .filter(entities::code_external_binding::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_external_binding::Column::SessionId.eq(session.0))
        .filter(entities::code_external_binding::Column::GrantId.eq(grant.0))
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    if bindings.is_empty() {
        return Ok(Vec::new());
    }
    for binding in bindings {
        require_live_scope(
            &store.conn,
            owner,
            session,
            grant,
            CodeBindingId(binding.id),
        )
        .await?;
    }
    let now = database_now(&store.conn).await?;
    let rows = entities::code_conversation_request::Entity::find()
        .filter(entities::code_conversation_request::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_conversation_request::Column::SessionId.eq(session.0))
        .filter(entities::code_conversation_request::Column::GrantId.eq(grant.0))
        .filter(entities::code_conversation_request::Column::Result.is_null())
        .filter(
            entities::code_conversation_request::Column::CreatedAt
                .gt(now - ConversationRequest::TTL),
        )
        .order_by_asc(entities::code_conversation_request::Column::CreatedAt)
        .order_by_asc(entities::code_conversation_request::Column::Id)
        .limit(ConversationRequest::MAX_PENDING_PER_SESSION)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(request_from_model)
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// Complete one request. Equal duplicate results are idempotent; a different
/// result on the completed row is a conflict so a retry cannot mix outcomes.
pub async fn complete_conversation_request(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    grant: CodeGrantId,
    request_id: uuid::Uuid,
    result: &Value,
) -> Result<Option<ConversationRequest>> {
    validate_bounded_json("conversation request result", result)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !super::acquire_code_session_write_lock(&transaction, session).await? {
        return Ok(None);
    }
    let Some(scope_row) = entities::code_conversation_request::Entity::find_by_id(request_id)
        .filter(entities::code_conversation_request::Column::Owner.eq(owner.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let row = scope_row;
    if row.session_id != session.0 || row.grant_id != grant.0 {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    require_live_scope(
        &transaction,
        owner,
        session,
        grant,
        CodeBindingId(row.binding_id),
    )
    .await?;
    let now = database_now(&transaction).await?;
    if now - row.created_at >= ConversationRequest::TTL {
        transaction.commit().await.map_err(store_err)?;
        return Err(AgentError::InvalidTarget(
            "the conversation request has expired".into(),
        ));
    }
    validate_operation_result(&row.operation, &row.arguments, result)?;
    if let Some(ref stored) = row.result {
        let stored: Value = stored.clone();
        let request = request_from_model(row)?;
        transaction.commit().await.map_err(store_err)?;
        if stored == *result {
            return Ok(Some(request));
        }
        return Err(AgentError::ConversationRequestConflict(
            "the request already has a different result".into(),
        ));
    }
    let updated = entities::code_conversation_request::Entity::update_many()
        .col_expr(
            entities::code_conversation_request::Column::Result,
            sea_orm::sea_query::Expr::value(
                serde_json::to_value(result).map_err(AgentError::Serde)?,
            ),
        )
        .col_expr(
            entities::code_conversation_request::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::code_conversation_request::Column::Id.eq(request_id))
        .filter(entities::code_conversation_request::Column::Result.is_null())
        .filter(entities::code_conversation_request::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_conversation_request::Column::SessionId.eq(session.0))
        .filter(entities::code_conversation_request::Column::GrantId.eq(grant.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let final_row = entities::code_conversation_request::Entity::find_by_id(request_id)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("request disappeared mid-completion".into()))?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(request_from_model(final_row)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn history() -> Value {
        json!({"source":"slack", "messages":[{"id":"1", "timestamp":"1.0", "author":{"id":"U1","name":"Mira","kind":"user"}, "text":"hello", "attachments":[{"id":"F1","name":"video.mp4","kind":"video","readable":false}]}], "next_cursor":null, "has_more":false, "truncated":false})
    }

    #[test]
    fn conversation_result_accepts_terminal_pages_and_unreadable_media() {
        validate_operation_result("read", &json!({"limit":1,"max_bytes":1024}), &history())
            .unwrap();
    }

    #[test]
    fn conversation_result_counts_all_serialized_bytes_including_metadata_and_errors() {
        let base = history();
        let exact = serde_json::to_vec(&base).unwrap().len();
        for operation in ["read", "export"] {
            validate_operation_result(operation, &json!({"max_bytes":exact}), &base).unwrap();
            assert!(
                validate_operation_result(operation, &json!({"max_bytes":exact - 1}), &base)
                    .unwrap_err()
                    .to_string()
                    .contains("serialized bytes")
            );
            for location in ["unknown", "metadata"] {
                let mut attack = base.clone();
                if location == "unknown" {
                    attack["unrecognized"] = json!("x".repeat(1024));
                } else {
                    attack["messages"][0]["author"]["extra"] = json!("x".repeat(1024));
                }
                assert!(
                    validate_operation_result(operation, &json!({"max_bytes":1024}), &attack)
                        .unwrap_err()
                        .to_string()
                        .contains("serialized bytes")
                );
            }
            let error = json!({"error":{"code":"failure","message":"x".repeat(1100)}});
            assert!(
                validate_operation_result(operation, &json!({"max_bytes":1024}), &error)
                    .unwrap_err()
                    .to_string()
                    .contains("serialized bytes")
            );
        }
        for (operation, hard_cap) in [
            ("read", ConversationRequest::READ_MAX_BYTES),
            ("export", ConversationRequest::EXPORT_MAX_BYTES),
        ] {
            let mut attack = base.clone();
            attack["unknown"] = json!("x".repeat(hard_cap));
            assert!(validate_operation_result(
                operation,
                &json!({"max_bytes":ConversationRequest::MAX_JSON_BYTES}),
                &attack
            )
            .is_err());
        }
    }

    #[test]
    fn conversation_attachment_uses_top_level_bytes_and_binds_requested_id() {
        let attachment =
            json!({"id":"F1", "name":"image.png", "mime_type":"image/png", "kind":"image"});
        let result = json!({"attachment":attachment, "data_base64":"aGVsbG8="});
        validate_operation_result("attachment", &json!({"attachment_id":"F1"}), &result).unwrap();
        assert!(
            validate_operation_result("attachment", &json!({"attachment_id":"F2"}), &result)
                .is_err()
        );
        let mut nested = json!({"attachment":attachment});
        nested["attachment"]["data_base64"] = json!("aGVsbG8=");
        assert!(
            validate_operation_result("attachment", &json!({"attachment_id":"F1"}), &nested)
                .is_err()
        );
        let oversized = json!({"attachment":attachment, "data_base64":base64::engine::general_purpose::STANDARD.encode(vec![0; ConversationRequest::ATTACHMENT_MAX_DECODED_BYTES + 1])});
        assert!(validate_operation_result(
            "attachment",
            &json!({"attachment_id":"F1"}),
            &oversized
        )
        .unwrap_err()
        .to_string()
        .contains("decodes"));
    }
}
