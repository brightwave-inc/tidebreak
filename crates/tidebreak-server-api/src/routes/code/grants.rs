//! Adapter grants and the connect handshake (docs/slack-sessions.md,
//! stage 2).
//!
//! Two audiences share this file, on two surfaces. The person, on the
//! authenticated API: the connect approval page (`view`/`approve`) and the
//! desktop grants list with revoke. The adapter holds no grant until connect
//! completes, so the external surface uses a narrow deployment bootstrap
//! bearer for start and a separate per-handshake confirmation capability for
//! status and completion. Approval alone mints nothing — a forwarded connect
//! link therefore binds nothing.

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};

use crate::auth::AdapterBootstrapAuth;
use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::state::AppState;

use super::types::{CodeConnectPage, CodeGrantSnapshot};

/// The code runtime for an adapter-facing connect handler, 404 when code
/// mode is not configured — the same shape as an invalid nonce, so the
/// surface says nothing about this machine's setup.
fn adapter_runtime(
    state: &AppState,
) -> Result<std::sync::Arc<crate::code::runtime::CodeRuntime>, ServerError> {
    state
        .code
        .clone()
        .ok_or_else(|| ServerError::not_found("this connect link is no longer valid"))
}

fn confirmation_token(headers: &HeaderMap) -> Result<&str, ServerError> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ServerError::not_found("this connect link is no longer valid"))
}

/// `GET /code/grants` — every grant the owner holds, revoked ones
/// included so a theft-triggered revoke and its reason stay visible.
pub async fn list_grants(code: ScopedCode) -> Result<Json<Vec<CodeGrantSnapshot>>, ServerError> {
    let grants = code.list_adapter_grants().await?;
    let mut profiles: std::collections::HashMap<_, _> = code
        .list_adapter_grant_profiles()
        .await?
        .into_iter()
        .map(|profile| (profile.grant_id, profile))
        .collect();
    Ok(Json(
        grants
            .into_iter()
            .map(|grant| {
                let profile = profiles.remove(&grant.id);
                CodeGrantSnapshot::from_grant_and_profile(grant, profile)
            })
            .collect(),
    ))
}

#[derive(serde::Deserialize)]
pub struct RevokeGrantBody {
    #[serde(default)]
    pub reason: Option<String>,
}

/// `POST /code/grants/{id}/revoke` — revoke one grant. Severs its live
/// event streams before answering.
pub async fn revoke_grant(
    code: ScopedCode,
    Path(id): Path<tidebreak_core::CodeGrantId>,
    Json(body): Json<RevokeGrantBody>,
) -> Result<Json<CodeGrantSnapshot>, ServerError> {
    let reason = body.reason.as_deref().unwrap_or("revoked by the owner");
    let grant = code
        .revoke_adapter_grant(id, reason)
        .await?
        .ok_or_else(|| ServerError::not_found("grant not found"))?;
    Ok(Json(CodeGrantSnapshot::from(grant)))
}

#[derive(serde::Deserialize)]
pub struct RevokeWorkspaceBody {
    pub channel_kind: String,
    pub workspace_identity: String,
}

/// `POST /code/grants/revoke-workspace` — revoke every live grant a
/// channel workspace holds, the whole-workspace cutoff the grants list
/// offers against a hostile workspace admin.
pub async fn revoke_workspace_grants(
    code: ScopedCode,
    Json(body): Json<RevokeWorkspaceBody>,
) -> Result<Json<Vec<CodeGrantSnapshot>>, ServerError> {
    let revoked = code
        .revoke_workspace_grants(
            &body.channel_kind,
            &body.workspace_identity,
            "the owner revoked the whole workspace",
        )
        .await?;
    Ok(Json(
        revoked.into_iter().map(CodeGrantSnapshot::from).collect(),
    ))
}

#[derive(serde::Deserialize)]
pub struct ConnectStartBody {
    pub channel_kind: String,
    pub external_identity: String,
    pub workspace_identity: String,
    pub display_name: String,
    pub workspace_name: String,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

#[derive(serde::Serialize)]
pub struct ConnectStartResponse {
    /// Goes into the connect card link, once; the machine keeps a hash.
    pub nonce: String,
    /// Adapter-only capability for status polling and the closing confirm.
    /// This value never appears in the approval link.
    pub confirmation_token: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// `GET /external/connect/probe` — the pairing check an operator's setup
/// page runs. It proves three things and changes nothing: the machine is
/// reachable, code mode is configured, and the presented bootstrap token
/// is one this deployment accepts. An adapter whose token the machine
/// refuses fails here, at setup time, instead of at a user's first
/// connect card.
pub async fn connect_probe(
    State(state): State<AppState>,
    _bootstrap: AdapterBootstrapAuth,
) -> Result<StatusCode, ServerError> {
    let _ = adapter_runtime(&state)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /external/connect` — the adapter parks a handshake and gets the
/// one-time nonce for its connect card. On the external surface: the
/// adapter holds no grant yet, and the handshake is inert until the owner
/// approves it on the authenticated page.
pub async fn connect_start(
    State(state): State<AppState>,
    _bootstrap: AdapterBootstrapAuth,
    Json(body): Json<ConnectStartBody>,
) -> Result<(StatusCode, Json<ConnectStartResponse>), ServerError> {
    let (handshake, nonce, confirmation_token) = adapter_runtime(&state)?
        .start_connect_handshake(
            &body.channel_kind,
            &body.external_identity,
            &body.workspace_identity,
            &body.display_name,
            &body.workspace_name,
            body.avatar_url.as_deref(),
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(ConnectStartResponse {
            nonce,
            confirmation_token,
            expires_at: handshake.expires_at,
        }),
    ))
}

/// `GET /external/connect/{nonce}` — what the approval page renders. A
/// used or stale link answers not-found and shows nothing.
pub async fn connect_view(
    code: ScopedCode,
    Path(nonce): Path<String>,
) -> Result<Json<CodeConnectPage>, ServerError> {
    let (handshake, csrf) = code
        .view_connect_handshake(&nonce)
        .await?
        .ok_or_else(|| ServerError::not_found("this connect link is no longer valid"))?;
    Ok(Json(CodeConnectPage {
        channel_kind: handshake.channel_kind,
        display_name: handshake.display_name,
        workspace_name: handshake.workspace_name,
        avatar_url: handshake.avatar_url,
        state: handshake.state.as_str().to_owned(),
        csrf,
        expires_at: handshake.expires_at,
    }))
}

#[derive(serde::Deserialize)]
pub struct ConnectApproveBody {
    pub csrf: String,
}

/// `POST /external/connect/{nonce}/approve` — the owner's "is this you?".
/// CSRF-protected; mints nothing by itself.
pub async fn connect_approve(
    code: ScopedCode,
    Path(nonce): Path<String>,
    lease: Option<axum::Extension<crate::auth::GatewayAuthLease>>,
    Json(body): Json<ConnectApproveBody>,
) -> Result<StatusCode, ServerError> {
    code.approve_connect_handshake(&nonce, &body.csrf, lease.as_ref().map(|lease| &lease.0))
        .await?
        .ok_or_else(|| ServerError::not_found("this connect link is no longer valid"))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Serialize)]
pub struct ConnectStatusResponse {
    /// `pending`, `approved`, or nothing once completed or expired — those
    /// answer not-found like an invalid nonce.
    pub state: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// `GET /external/connect/{nonce}/status` — how the adapter learns the
/// owner approved, so it can send its DM confirm. On the external surface,
/// gated by the nonce; answers state and expiry only, never the CSRF token
/// the authenticated approval page gets.
pub async fn connect_status(
    State(state): State<AppState>,
    Path(nonce): Path<String>,
    headers: HeaderMap,
) -> Result<Json<ConnectStatusResponse>, ServerError> {
    let confirmation_token = confirmation_token(&headers)?;
    let handshake = adapter_runtime(&state)?
        .connect_handshake_status(&nonce, confirmation_token)
        .await?
        .ok_or_else(|| ServerError::not_found("this connect link is no longer valid"))?;
    Ok(Json(ConnectStatusResponse {
        state: handshake.state.as_str().to_owned(),
        expires_at: handshake.expires_at,
    }))
}

#[derive(serde::Serialize)]
pub struct ConnectCompleteResponse {
    pub grant: CodeGrantSnapshot,
    /// The only copy of the pair; the machine keeps hashes.
    pub token: String,
    pub refresh: String,
}

/// `POST /external/connect/{nonce}/complete` — the adapter's closing
/// confirm after its DM proved control of the channel account. On the
/// external surface, gated by the one-time nonce it consumes; mints the
/// grant bound to the identity the page showed.
pub async fn connect_complete(
    State(state): State<AppState>,
    Path(nonce): Path<String>,
    headers: HeaderMap,
) -> Result<Json<ConnectCompleteResponse>, ServerError> {
    let confirmation_token = confirmation_token(&headers)?;
    let (grant, pair) = adapter_runtime(&state)?
        .complete_connect_handshake(&nonce, confirmation_token)
        .await?
        .ok_or_else(|| ServerError::not_found("this connect link is no longer valid"))?;
    Ok(Json(ConnectCompleteResponse {
        grant: CodeGrantSnapshot::from(grant),
        token: pair.token,
        refresh: pair.refresh,
    }))
}
