//! Native tool receipts fence replay before any side effect runs.
use super::super::super::{entities, store_err, DbStore};
use crate::code::{CodeIncarnationId, SessionId};
use crate::error::{AgentError, Result};
use crate::{CallId, OwnerId};
use entities::code_native_tool_receipt as receipt;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};
use serde_json::Value;

/// A running request is never automatically reset after a crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeToolStatus {
    /// Enqueued, with no execution claimed.
    Pending,
    /// Execution may have performed a side effect.
    Running,
    /// The exact result is durable.
    Completed,
}
/// A stored request; private fields prevent callers from inventing authority.
#[derive(Debug, Clone)]
pub struct NativeToolReceipt {
    id: uuid::Uuid,
    owner: String,
    session: SessionId,
    incarnation: CodeIncarnationId,
    grant: uuid::Uuid,
    /// Stable remote request identifier.
    pub request_id: String,
    /// Stable native tool invocation identifier.
    pub call_id: CallId,
    /// Exact registered tool name.
    pub tool: String,
    /// Exact JSON arguments.
    pub arguments: Value,
    /// Stored result, including bounded base64 artifacts.
    pub result: Option<Value>,
    /// Execution state.
    pub status: NativeToolStatus,
    /// The remote transport accepted the complete result.
    pub delivered: bool,
}
/// Only `Claimed` permits execution.
#[derive(Debug)]
pub enum NativeToolClaim {
    /// This transaction owns the first execution.
    Claimed(NativeToolReceipt),
    /// Another execution started; its outcome may be uncertain.
    Running(NativeToolReceipt),
    /// Reuse the durable result.
    Completed(NativeToolReceipt),
}
fn invalid(message: &str) -> AgentError {
    AgentError::InvalidTarget(message.into())
}
fn convert(row: receipt::Model) -> Result<NativeToolReceipt> {
    Ok(NativeToolReceipt {
        id: row.id,
        owner: row.owner,
        session: SessionId(row.session_id),
        incarnation: CodeIncarnationId(row.incarnation_id),
        grant: row.grant_id,
        request_id: row.request_id,
        call_id: CallId(row.call_id),
        tool: row.tool,
        arguments: row.arguments,
        result: row.result,
        status: match row.status.as_str() {
            "pending" => NativeToolStatus::Pending,
            "running" => NativeToolStatus::Running,
            "completed" => NativeToolStatus::Completed,
            _ => return Err(invalid("invalid native tool receipt status")),
        },
        delivered: row.delivered,
    })
}
fn bounded(value: &Value, cap: usize) -> Result<()> {
    if serde_json::to_vec(value)?.len() > cap {
        return Err(invalid("native tool payload exceeds its byte limit"));
    }
    Ok(())
}
/// Lock authority rows before reading so revocation and stop serialize with admission.
async fn scope<C: sea_orm::ConnectionTrait>(
    conn: &C,
    owner: &OwnerId,
    session: SessionId,
    incarnation: CodeIncarnationId,
) -> Result<uuid::Uuid> {
    if !super::acquire_code_session_write_lock(conn, session).await? {
        return Err(invalid("native tool session is absent"));
    }
    let session_row = entities::session::Entity::find_by_id(session.0)
        .filter(entities::session::Column::Owner.eq(owner.as_str()))
        .one(conn)
        .await
        .map_err(store_err)?;
    if session_row.is_none_or(|s| ["ended", "fenced"].contains(&s.lifecycle.as_str())) {
        return Err(invalid("native tool session is no longer live"));
    }
    use entities::code_session_incarnation as inc;
    let locked = inc::Entity::update_many()
        .col_expr(
            inc::Column::State,
            sea_orm::sea_query::Expr::col(inc::Column::State),
        )
        .filter(inc::Column::Id.eq(incarnation.0))
        .filter(inc::Column::Owner.eq(owner.as_str()))
        .filter(inc::Column::SessionId.eq(session.0))
        .filter(inc::Column::State.eq("active"))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if locked.rows_affected != 1 {
        return Err(invalid("native tool incarnation is no longer active"));
    }
    let newest = inc::Entity::find()
        .filter(inc::Column::Owner.eq(owner.as_str()))
        .filter(inc::Column::SessionId.eq(session.0))
        .order_by_desc(inc::Column::Incarnation)
        .one(conn)
        .await
        .map_err(store_err)?;
    if newest.is_none_or(|i| i.id != incarnation.0) {
        return Err(invalid("native tool incarnation is superseded"));
    }
    use entities::code_external_binding as binding;
    let binding = binding::Entity::find()
        .filter(binding::Column::Owner.eq(owner.as_str()))
        .filter(binding::Column::SessionId.eq(session.0))
        .order_by_asc(binding::Column::CreatedAt)
        .order_by_asc(binding::Column::Id)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| invalid("native tool needs an originating channel binding"))?;
    let other_grants = binding::Entity::find()
        .filter(binding::Column::Owner.eq(owner.as_str()))
        .filter(binding::Column::SessionId.eq(session.0))
        .filter(binding::Column::GrantId.ne(binding.grant_id))
        .count(conn)
        .await
        .map_err(store_err)?;
    if other_grants != 0 {
        return Err(invalid("native tool needs one originating channel grant"));
    }
    let locked = binding::Entity::update_many()
        .col_expr(
            binding::Column::GrantId,
            sea_orm::sea_query::Expr::col(binding::Column::GrantId),
        )
        .filter(binding::Column::Id.eq(binding.id))
        .filter(binding::Column::GrantId.eq(binding.grant_id))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if locked.rows_affected != 1 {
        return Err(invalid("native tool channel binding changed"));
    }
    use entities::code_external_grant as grant;
    let locked = grant::Entity::update_many()
        .col_expr(
            grant::Column::Kind,
            sea_orm::sea_query::Expr::col(grant::Column::Kind),
        )
        .filter(grant::Column::Id.eq(binding.grant_id))
        .filter(grant::Column::Owner.eq(owner.as_str()))
        .filter(grant::Column::RevokedAt.is_null())
        .exec(conn)
        .await
        .map_err(store_err)?;
    if locked.rows_affected != 1 {
        return Err(invalid("native tool originating grant is no longer live"));
    }
    Ok(binding.grant_id)
}
async fn load<C: sea_orm::ConnectionTrait>(
    conn: &C,
    owner: &OwnerId,
    prior: &NativeToolReceipt,
) -> Result<NativeToolReceipt> {
    if prior.owner != owner.as_str() {
        return Err(invalid("native tool receipt belongs to another owner"));
    }
    let grant = scope(conn, owner, prior.session, prior.incarnation).await?;
    if grant != prior.grant {
        return Err(invalid("native tool originating grant changed"));
    }
    let row = receipt::Entity::find_by_id(prior.id)
        .filter(receipt::Column::Owner.eq(owner.as_str()))
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| invalid("native tool receipt is absent"))?;
    convert(row)
}
/// Persist a request before advancing its remote event cursor.
#[allow(clippy::too_many_arguments)]
pub async fn enqueue_native_tool_request(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    incarnation: CodeIncarnationId,
    request_id: &str,
    tool: &str,
    arguments: &Value,
) -> Result<NativeToolReceipt> {
    if request_id.is_empty() || request_id.len() > 512 || tool.is_empty() || tool.len() > 128 {
        return Err(invalid(
            "native tool request identifiers exceed their limits",
        ));
    }
    bounded(arguments, 65_536)?;
    let tx = store.conn.begin().await.map_err(store_err)?;
    let grant = scope(&tx, owner, session, incarnation).await?;
    let prior = receipt::Entity::find()
        .filter(receipt::Column::Owner.eq(owner.as_str()))
        .filter(receipt::Column::SessionId.eq(session.0))
        .filter(receipt::Column::IncarnationId.eq(incarnation.0))
        .filter(receipt::Column::RequestId.eq(request_id))
        .one(&tx)
        .await
        .map_err(store_err)?;
    if let Some(row) = prior {
        if row.tool != tool || row.arguments != *arguments || row.grant_id != grant {
            return Err(invalid(
                "native tool request replay changed its payload or grant",
            ));
        }
        tx.commit().await.map_err(store_err)?;
        return convert(row);
    }
    let outstanding = receipt::Entity::find()
        .filter(receipt::Column::Owner.eq(owner.as_str()))
        .filter(receipt::Column::SessionId.eq(session.0))
        .filter(receipt::Column::IncarnationId.eq(incarnation.0))
        .filter(receipt::Column::Delivered.eq(false))
        .count(&tx)
        .await
        .map_err(store_err)?;
    if outstanding >= 64 {
        return Err(invalid("native tool request queue is full"));
    }
    let row = receipt::ActiveModel {
        id: Set(uuid::Uuid::new_v4()),
        owner: Set(owner.to_string()),
        session_id: Set(session.0),
        incarnation_id: Set(incarnation.0),
        grant_id: Set(grant),
        request_id: Set(request_id.to_owned()),
        call_id: Set(CallId::new().0),
        tool: Set(tool.to_owned()),
        arguments: Set(arguments.clone()),
        result: Set(None),
        status: Set("pending".into()),
        delivered: Set(false),
    }
    .insert(&tx)
    .await
    .map_err(store_err)?;
    tx.commit().await.map_err(store_err)?;
    convert(row)
}
/// Return queued, uncertain, and completed undelivered work for one live incarnation.
pub async fn list_native_tool_requests(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    incarnation: CodeIncarnationId,
) -> Result<Vec<NativeToolReceipt>> {
    let tx = store.conn.begin().await.map_err(store_err)?;
    let grant = scope(&tx, owner, session, incarnation).await?;
    let rows = receipt::Entity::find()
        .filter(receipt::Column::Owner.eq(owner.as_str()))
        .filter(receipt::Column::SessionId.eq(session.0))
        .filter(receipt::Column::IncarnationId.eq(incarnation.0))
        .filter(receipt::Column::GrantId.eq(grant))
        .filter(receipt::Column::Delivered.eq(false))
        .order_by_asc(receipt::Column::Id)
        .limit(64)
        .all(&tx)
        .await
        .map_err(store_err)?;
    tx.commit().await.map_err(store_err)?;
    rows.into_iter().map(convert).collect()
}
/// Claim once. A running receipt never expires into another execution.
pub async fn claim_native_tool_request(
    store: &DbStore,
    owner: &OwnerId,
    prior: &NativeToolReceipt,
) -> Result<NativeToolClaim> {
    let tx = store.conn.begin().await.map_err(store_err)?;
    let mut row = load(&tx, owner, prior).await?;
    let outcome = match row.status {
        NativeToolStatus::Pending => {
            let updated = receipt::Entity::update_many()
                .col_expr(
                    receipt::Column::Status,
                    sea_orm::sea_query::Expr::value("running"),
                )
                .filter(receipt::Column::Id.eq(row.id))
                .filter(receipt::Column::Status.eq("pending"))
                .exec(&tx)
                .await
                .map_err(store_err)?;
            if updated.rows_affected != 1 {
                return Err(invalid("native tool execution claim changed concurrently"));
            }
            row.status = NativeToolStatus::Running;
            NativeToolClaim::Claimed(row)
        }
        NativeToolStatus::Running => NativeToolClaim::Running(row),
        NativeToolStatus::Completed => NativeToolClaim::Completed(row),
    };
    tx.commit().await.map_err(store_err)?;
    Ok(outcome)
}
/// Preserve the exact bounded result before trying to send it to the sandbox.
pub async fn complete_native_tool_request(
    store: &DbStore,
    owner: &OwnerId,
    prior: &NativeToolReceipt,
    result: &Value,
) -> Result<NativeToolReceipt> {
    bounded(result, 3 * 1024 * 1024)?;
    let tx = store.conn.begin().await.map_err(store_err)?;
    let mut row = load(&tx, owner, prior).await?;
    validate_result(&row.request_id, result)?;
    if row.status == NativeToolStatus::Completed {
        if row.result.as_ref() != Some(result) {
            return Err(invalid("native tool result replay changed its payload"));
        }
    } else {
        if row.status != NativeToolStatus::Running {
            return Err(invalid("native tool execution has not been claimed"));
        }
        receipt::Entity::update_many()
            .col_expr(
                receipt::Column::Status,
                sea_orm::sea_query::Expr::value("completed"),
            )
            .col_expr(
                receipt::Column::Result,
                sea_orm::sea_query::Expr::value(result.clone()),
            )
            .filter(receipt::Column::Id.eq(row.id))
            .exec(&tx)
            .await
            .map_err(store_err)?;
        row.status = NativeToolStatus::Completed;
        row.result = Some(result.clone());
    }
    tx.commit().await.map_err(store_err)?;
    Ok(row)
}
/// Mark delivery only after all bounded frames have been accepted.
pub async fn mark_native_tool_request_delivered(
    store: &DbStore,
    owner: &OwnerId,
    prior: &NativeToolReceipt,
) -> Result<()> {
    let tx = store.conn.begin().await.map_err(store_err)?;
    let row = load(&tx, owner, prior).await?;
    if row.status != NativeToolStatus::Completed {
        return Err(invalid("native tool result is not complete"));
    }
    receipt::Entity::update_many()
        .col_expr(
            receipt::Column::Delivered,
            sea_orm::sea_query::Expr::value(true),
        )
        .filter(receipt::Column::Id.eq(row.id))
        .exec(&tx)
        .await
        .map_err(store_err)?;
    tx.commit().await.map_err(store_err)
}

fn validate_result(request_id: &str, result: &Value) -> Result<()> {
    use base64::Engine as _;
    if result.get("request_id").and_then(Value::as_str) != Some(request_id) {
        return Err(invalid("native tool result names another request"));
    }
    bounded(
        result
            .get("output")
            .ok_or_else(|| invalid("native tool result output is absent"))?,
        65_536,
    )?;
    let artifacts = result
        .get("artifacts")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("native tool result artifacts must be an array"))?;
    if artifacts.len() > 16 {
        return Err(invalid("native tool result has too many artifacts"));
    }
    let mut decoded = 0usize;
    for artifact in artifacts {
        let path = artifact
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("native tool artifact path is absent"))?;
        if path.is_empty()
            || path.len() > 4096
            || path.starts_with('/')
            || path.contains('\\')
            || path.split('/').any(|p| p == ".." || p.is_empty())
        {
            return Err(invalid("native tool artifact path must stay relative"));
        }
        let media = artifact
            .get("media_type")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("native tool artifact media type is absent"))?;
        if media.is_empty() || media.len() > 256 {
            return Err(invalid("native tool artifact media type exceeds its limit"));
        }
        let bytes = artifact
            .get("bytes")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("native tool artifact bytes must be base64"))?;
        decoded += base64::engine::general_purpose::STANDARD
            .decode(bytes)
            .map_err(|_| invalid("native tool artifact bytes must be base64"))?
            .len();
        if decoded > 2 * 1024 * 1024 {
            return Err(invalid("native tool artifact bytes exceed their limit"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_result;
    use serde_json::json;

    #[test]
    fn native_tool_receipts_bound_artifacts_and_bind_result_identity() {
        let result = json!({"request_id":"one", "output":{"ok":true}, "artifacts":[{"path":"exports/thread.txt", "media_type":"text/plain", "bytes":"aGVsbG8="}]});
        validate_result("one", &result).unwrap();
        assert!(validate_result("two", &result).is_err());
        for path in [
            "../secret",
            "/absolute",
            "exports/../../secret",
            "exports\\secret",
        ] {
            let mut changed = result.clone();
            changed["artifacts"][0]["path"] = json!(path);
            assert!(validate_result("one", &changed).is_err());
        }
        let mut changed = result.clone();
        changed["artifacts"][0]["bytes"] = json!([104, 101, 108, 108, 111]);
        assert!(validate_result("one", &changed).is_err());
        changed = result.clone();
        changed["output"] = json!("x".repeat(65_536));
        assert!(validate_result("one", &changed).is_err());
        changed = result.clone();
        changed["artifacts"] = json!(vec![result["artifacts"][0].clone(); 17]);
        assert!(validate_result("one", &changed).is_err());
    }
}
