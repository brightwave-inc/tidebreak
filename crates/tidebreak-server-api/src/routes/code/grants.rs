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
async fn snapshot_grant(
    code: &ScopedCode,
    grant: tidebreak_core::CodeExternalGrant,
    profile: Option<tidebreak_core::CodeGrantProfile>,
) -> Result<CodeGrantSnapshot, ServerError> {
    let mut snapshot = CodeGrantSnapshot::from_grant_and_profile(grant.clone(), profile);
    if grant.kind.is_workspace() {
        let channels = code
            .list_channel_repository_confirms(&grant.owner, grant.id)
            .await?
            .into_iter()
            .map(|row| crate::code::types::CodeGrantChannelSnapshot {
                channel_id: row.channel_id,
                repository: row.repository,
                state: row.state.as_str().to_owned(),
                set_by_identity: row.set_by_identity,
                set_by_display: row.set_by_display,
            })
            .collect();
        snapshot = snapshot.with_channels(channels);
    }
    Ok(snapshot)
}

/// `GET /code/grants` — every grant the owner holds, revoked ones
/// included so a theft-triggered revoke and its reason stay visible.
/// Admins also see workspace grants owned by the service principal.
pub async fn list_grants(code: ScopedCode) -> Result<Json<Vec<CodeGrantSnapshot>>, ServerError> {
    let mut grants = code.list_adapter_grants().await?;
    if code.is_admin() {
        for grant in code.list_workspace_grants_as_admin().await? {
            if !grants.iter().any(|existing| existing.id == grant.id) {
                grants.push(grant);
            }
        }
    }
    let mut profiles: std::collections::HashMap<_, _> = code
        .list_adapter_grant_profiles()
        .await?
        .into_iter()
        .map(|profile| (profile.grant_id, profile))
        .collect();
    if code.is_admin() {
        for profile in code.list_workspace_grant_profiles_as_admin().await? {
            profiles.entry(profile.grant_id).or_insert(profile);
        }
    }
    let mut snapshots = Vec::new();
    for grant in grants {
        let profile = profiles.remove(&grant.id);
        snapshots.push(snapshot_grant(&code, grant, profile).await?);
    }
    Ok(Json(snapshots))
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
    let grant = if code.is_admin() {
        match code.revoke_adapter_grant_as_admin(id, reason).await? {
            Some(grant) => grant,
            None => code
                .revoke_adapter_grant(id, reason)
                .await?
                .ok_or_else(|| ServerError::not_found("grant not found"))?,
        }
    } else {
        code.revoke_adapter_grant(id, reason)
            .await?
            .ok_or_else(|| ServerError::not_found("grant not found"))?
    };
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

#[derive(serde::Deserialize)]
pub struct WorkspaceGrantStartBody {
    pub channel_kind: String,
    pub workspace_identity: String,
    pub display: String,
}

/// `POST /code/grants/workspace` — a service principal starts a workspace
/// handshake. The adapter then polls status and completes as for a person.
pub async fn start_workspace_grant(
    code: ScopedCode,
    lease: Option<axum::Extension<crate::auth::GatewayAuthLease>>,
    Json(body): Json<WorkspaceGrantStartBody>,
) -> Result<(StatusCode, Json<WorkspaceGrantStartResponse>), ServerError> {
    let (handshake, nonce, confirmation_token) = code
        .start_workspace_handshake(
            &body.channel_kind,
            &body.workspace_identity,
            &body.display,
            lease.as_ref().map(|lease| &lease.0),
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(WorkspaceGrantStartResponse {
            id: handshake.id,
            nonce,
            confirmation_token,
            expires_at: handshake.expires_at,
        }),
    ))
}

#[derive(serde::Serialize)]
pub struct WorkspaceGrantStartResponse {
    pub id: tidebreak_core::CodeHandshakeId,
    pub nonce: String,
    pub confirmation_token: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// `GET /deployment/code/grants/workspace/{id}` — what the admin approval
/// page renders.
pub async fn view_workspace_grant(
    State(state): State<AppState>,
    Path(id): Path<tidebreak_core::CodeHandshakeId>,
) -> Result<Json<CodeConnectPage>, ServerError> {
    let runtime = adapter_runtime(&state)?;
    let (handshake, csrf) = runtime
        .view_workspace_handshake(id)
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
pub struct WorkspaceApproveBody {
    pub csrf: String,
}

/// `POST /deployment/code/grants/workspace/{id}/approve`
pub async fn approve_workspace_grant(
    code: ScopedCode,
    Path(id): Path<tidebreak_core::CodeHandshakeId>,
    Json(body): Json<WorkspaceApproveBody>,
) -> Result<StatusCode, ServerError> {
    code.approve_workspace_handshake(id, &body.csrf)
        .await?
        .ok_or_else(|| ServerError::not_found("this connect link is no longer valid"))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
pub struct ConfirmRepositoryBody {
    pub repository: String,
}

/// `POST /deployment/code/grants/workspace/{id}/channels/{channel_id}/repositories/confirm`
pub async fn confirm_workspace_channel_repository(
    code: ScopedCode,
    Path((id, channel_id)): Path<(tidebreak_core::CodeGrantId, String)>,
    Json(body): Json<ConfirmRepositoryBody>,
) -> Result<StatusCode, ServerError> {
    code.confirm_workspace_channel_repository(id, &channel_id, &body.repository)
        .await?
        .ok_or_else(|| ServerError::not_found("repository confirmation not found"))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApproveRepositoriesBody {
    pub repositories: Vec<String>,
}

/// `POST /deployment/code/grants/workspace/{id}/channels/{channel_id}/repositories/approve`
pub async fn approve_workspace_channel_repositories(
    code: ScopedCode,
    Path((id, channel_id)): Path<(tidebreak_core::CodeGrantId, String)>,
    Json(body): Json<ApproveRepositoriesBody>,
) -> Result<StatusCode, ServerError> {
    if !code
        .approve_workspace_channel_repositories(id, &channel_id, &body.repositories)
        .await?
    {
        return Err(ServerError::not_found("workspace grant not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}
