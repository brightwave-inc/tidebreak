use axum::http::StatusCode;

use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::{Json, Path};
use tidebreak_core::SessionId;

use super::types::{
    AddSessionAccessBody, SessionAccessSnapshot, SessionSnapshot, SetSessionVisibilityBody,
};

pub async fn list_session_access(
    code: ScopedCode,
    Path(id): Path<SessionId>,
) -> Result<Json<Vec<SessionAccessSnapshot>>, ServerError> {
    Ok(Json(
        code.list_session_access(id)
            .await?
            .into_iter()
            .map(SessionAccessSnapshot::from)
            .collect(),
    ))
}

pub async fn add_session_access(
    code: ScopedCode,
    Path(id): Path<SessionId>,
    Json(body): Json<AddSessionAccessBody>,
) -> Result<(StatusCode, Json<SessionAccessSnapshot>), ServerError> {
    let row = code
        .grant_session_access(id, &body.subject, body.level)
        .await?;
    Ok((StatusCode::CREATED, Json(SessionAccessSnapshot::from(row))))
}

pub async fn revoke_session_access(
    code: ScopedCode,
    Path((id, subject)): Path<(SessionId, String)>,
) -> Result<StatusCode, ServerError> {
    if code.revoke_session_access(id, &subject).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ServerError::not_found("session access row not found"))
    }
}

pub async fn set_session_visibility(
    code: ScopedCode,
    Path(id): Path<SessionId>,
    Json(body): Json<SetSessionVisibilityBody>,
) -> Result<Json<SessionSnapshot>, ServerError> {
    Ok(Json(SessionSnapshot::from(
        code.set_session_visibility(id, body.visibility).await?,
    )))
}
