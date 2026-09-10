//! The route surface a channel adapter speaks (docs/slack-sessions.md,
//! stage 2): external get-or-create, messages, session events over
//! WebSocket, interrupt, and reap — nothing else. No settings, no
//! repository administration.
//!
//! Every route authenticates by adapter token, and every session route
//! then checks that the token's grant tags the session through a binding.
//! A session another grant holds answers "not found", the same shape as a
//! session that does not exist, so the surface leaks nothing about other
//! grants' work.

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::Response;

use tidebreak_core::{
    ApprovalId, CodeExternalGrant, ExternalSessionResolution, GrantRotation, HarnessKind,
    PermissionMode, RepoId, SessionId,
};

use crate::code::runtime::{ExternalMessage, ExternalMessageOutcome, NewSessionSettings};
use crate::error::ServerError;
use crate::extract::{Json, Path, Query};
use crate::state::AppState;

use super::types::{
    ApprovalDecision, ApprovalSnapshot, QueuedTurn, SessionEventsQuery, SessionSnapshot,
};
use crate::code::runtime::ApprovalDecisionRequest;

/// The authenticated grant behind an adapter request.
///
/// Fails closed: no code runtime, no bearer token, or a token matching no
/// live grant all answer 401 before any handler runs. A revoked grant's
/// token stops matching, so its next call dies here — the shape the grant
/// slice promises.
pub struct ExternalGrantAuth(pub CodeExternalGrant);

impl FromRequestParts<AppState> for ExternalGrantAuth {
    type Rejection = ServerError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Some(runtime) = state.code.clone() else {
            return Err(ServerError::unauthorized(
                "adapter access is not configured",
            ));
        };
        let token = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| ServerError::unauthorized("an adapter token is required"))?;
        let grant = runtime
            .authenticate_adapter_token(token)
            .await?
            .ok_or_else(|| ServerError::unauthorized("the adapter token matches no live grant"))?;
        Ok(Self(grant))
    }
}

/// Refuse a session the grant does not tag, with the not-found shape.
pub(super) async fn require_bound(
    state: &AppState,
    grant: &CodeExternalGrant,
    session: SessionId,
) -> Result<std::sync::Arc<crate::code::runtime::CodeRuntime>, ServerError> {
    let runtime = state
        .code
        .clone()
        .ok_or_else(|| ServerError::unauthorized("adapter access is not configured"))?;
    let bound = tidebreak_core::db::code::session_bound_to_grant(
        &runtime.db,
        &grant.owner,
        session,
        grant.id,
    )
    .await?;
    if !bound {
        return Err(ServerError::not_found("code session not found"));
    }
    Ok(runtime)
}

#[derive(serde::Deserialize)]
pub struct ExternalSessionBody {
    /// The channel's durable conversation identity, opaque here.
    pub external_key: String,
    /// An optional repository workspace. With neither selector, create a
    /// conversation in private scratch. `repo_id` wins when both are sent.
    #[serde(default)]
    pub repo_id: Option<RepoId>,
    /// The repository by its origin, `owner/name`, resolved against the
    /// grant owner's registered repositories. This is how a channel names
    /// its default: the adapter never learns record ids.
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// Select an engine explicitly. Without one, repository work uses Claude
    /// Code and repositoryless conversations use the internal engine.
    #[serde(default)]
    pub harness: Option<HarnessKind>,
    /// The permission mode the channel asks for. On the machine's engine it
    /// is honored up to the operator's ceiling and refused by name above it;
    /// absent, the session takes the operator's default (decision 88). A
    /// sandbox session is always `allow`, so any other value is refused.
    #[serde(default)]
    pub permission_mode: Option<PermissionMode>,
    /// Required under a workspace grant.
    #[serde(default)]
    pub channel_id: Option<String>,
    /// Whose forge identity this conversation should act as. A service-owned
    /// session or a workspace grant ignores this and always acts as the bot.
    #[serde(default)]
    pub acts_as: Option<tidebreak_core::ActsAs>,
}

#[derive(serde::Serialize)]
pub struct ExternalSessionResponse {
    pub harness: HarnessKind,
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings_path: Option<String>,
    /// `created`, `existing`, or `ended`.
    pub status: &'static str,
    pub session_id: SessionId,
    /// The conversation binding to name when sending first-turn context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_id: Option<tidebreak_core::CodeBindingId>,
    pub acts_as: tidebreak_core::ActsAs,
    /// The person's login or the App's bot login, when the forge named one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acting_login: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    /// Present only when the person could connect and did not, so the session
    /// is running as the bot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connect_url: Option<String>,
}

/// Freeze the chat default against the connection that admitted this session.
/// Browser credentials never select or authorize an external conversation's model.
#[cfg(test)]
pub(crate) async fn resolve_external_model(
    state: &AppState,
    grant: &CodeExternalGrant,
) -> Result<String, ServerError> {
    resolve_external_model_selection(state, grant, None).await
}

/// Preserve the model identifier that the selected harness sends to its provider.
/// Only the internal engine understands Tidebreak's frozen chat selectors.
async fn resolve_external_harness_model(
    state: &AppState,
    grant: &CodeExternalGrant,
    harness: HarnessKind,
    selection: Option<&str>,
) -> Result<Option<String>, ServerError> {
    if harness.is_in_process() {
        return resolve_external_model_selection(state, grant, selection)
            .await
            .map(Some);
    }
    let Some(selection) = selection else {
        return Ok(None);
    };
    let runtime = state
        .code
        .as_ref()
        .ok_or_else(|| ServerError::unauthorized("adapter access is not configured"))?;
    if let Some(relay) = runtime.harness_llm() {
        let (anthropic, openai) = match relay.external_delegations() {
            Some(external) => {
                external
                    .for_grant(&grant.owner, grant.id)
                    .await?
                    .compat_listings(&grant.owner)
                    .await?
            }
            None => relay.listings(&grant.owner).await?,
        };
        return select_external_compat_model(harness, selection, anthropic, openai).map(Some);
    }
    Ok(Some(selection.to_owned()))
}

fn select_external_compat_model(
    harness: HarnessKind,
    selection: &str,
    anthropic: tidebreak_core::Result<Vec<crate::obo_gateway::GatewayCompatModel>>,
    openai: tidebreak_core::Result<Vec<crate::obo_gateway::GatewayCompatModel>>,
) -> Result<String, ServerError> {
    let (models, id) = match harness {
        HarnessKind::ClaudeCode => (anthropic?, selection),
        HarnessKind::Codex | HarnessKind::Grok => (openai?, selection),
        HarnessKind::Opencode => {
            if let Some(id) = selection.strip_prefix("anthropic/") {
                (anthropic?, id)
            } else if let Some(id) = selection.strip_prefix("model-gateway/") {
                (openai?, id)
            } else {
                return Err(unavailable_external_harness_model());
            }
        }
        HarnessKind::Internal => return Err(unavailable_external_harness_model()),
    };
    if !models.iter().any(|model| model.id == id) {
        return Err(unavailable_external_harness_model());
    }
    Ok(selection.to_owned())
}

fn unavailable_external_harness_model() -> ServerError {
    ServerError::conflict_kind(
        "model_provider_unavailable",
        "this Slack connection cannot use the selected harness model; choose an available model in channel settings",
    )
}

async fn resolve_external_model_selection(
    state: &AppState,
    grant: &CodeExternalGrant,
    selection: Option<&str>,
) -> Result<String, ServerError> {
    use crate::model_roles::{self, ModelRole};
    let (selected, explicit) = if let Some(model) = selection {
        (model.to_owned(), true)
    } else {
        match model_roles::read_selection(&*state.store, ModelRole::Chat).await? {
            Some(model) => (model, true),
            None => (state.agent_config.model.clone(), false),
        }
    };
    let runtime = state
        .code
        .as_ref()
        .ok_or_else(|| ServerError::unauthorized("adapter access is not configured"))?;
    let snapshot = match runtime
        .harness_llm()
        .and_then(|relay| relay.external_delegations().cloned())
    {
        Some(external) => {
            let gateway = external.for_grant(&grant.owner, grant.id).await.map_err(|error| {
                match error {
                    tidebreak_core::AgentError::SignInRequired(_)
                    | tidebreak_core::AgentError::InvalidTarget(_) => ServerError::conflict_kind(
                        "external_reconnect_required",
                        "Your Slack connection needs approval again. Send `reconnect` to Tidebreak, then approve the connection.",
                    ),
                    error => ServerError::from(error),
                }
            })?;
            Some(gateway.snapshot_for(&grant.owner).await?.ok_or_else(|| {
                ServerError::conflict_kind(
                    "model_provider_unavailable",
                    "this Slack connection has no available model catalog; reconnect it from Slack",
                )
            })?)
        }
        None => None,
    };
    if !state.resolver.enforces_model_registry() && snapshot.is_none() {
        return Ok(selected);
    }
    let managed = state.managed_policy()?;
    let executable = if managed.managed || snapshot.is_some() {
        model_roles::effective_chat_policy(
            &*state.store,
            &*state.secrets,
            &managed,
            &selected,
            explicit,
            snapshot.as_ref(),
        )
        .await?
        .ok_or_else(|| {
            ServerError::conflict_kind(
            "model_provider_unavailable",
            "this Slack connection has no available default model; choose an entitled chat model",
        )
        })?
        .execution_key()
    } else {
        selected
    };
    super::super::providers_models::validate_execution_selection(
        state,
        &executable,
        true,
        snapshot.as_ref(),
    )
    .await
}

/// Recover a source-context write only through the original conversation.
/// An attached destination must never become the session's origin.
async fn repair_original_context(
    runtime: &crate::code::runtime::CodeRuntime,
    grant: &CodeExternalGrant,
    binding: &tidebreak_core::CodeExternalBinding,
    channel: Option<&str>,
) -> Result<(), ServerError> {
    if tidebreak_core::db::code::session_context(&runtime.db, &grant.owner, binding.session_id)
        .await?
        .is_some()
    {
        return Ok(());
    }
    let bindings = tidebreak_core::db::code::list_bindings_for_session(
        &runtime.db,
        &grant.owner,
        binding.session_id,
    )
    .await?;
    if bindings
        .first()
        .is_some_and(|original| original.id == binding.id)
    {
        tidebreak_core::db::code::set_session_context(
            &runtime.db,
            &grant.owner,
            binding.session_id,
            channel,
            None,
            None,
        )
        .await?;
    }
    Ok(())
}

/// `POST /external/code/sessions` — idempotent get-or-create for one
/// conversation. An ended session answers `ended` rather than
/// resurrecting; a conversation bound under another grant answers "not
/// found" like any other session outside this grant's scope.
pub async fn external_get_or_create(
    State(state): State<AppState>,
    ExternalGrantAuth(grant): ExternalGrantAuth,
    Json(body): Json<ExternalSessionBody>,
) -> Result<(StatusCode, Json<ExternalSessionResponse>), ServerError> {
    let runtime = state
        .code
        .clone()
        .ok_or_else(|| ServerError::unauthorized("adapter access is not configured"))?;
    if grant.channel_kind.trim().is_empty() || body.external_key.trim().is_empty() {
        return Err(ServerError::conflict_kind(
            "binding_key_invalid",
            "a binding needs a channel kind and a conversation key",
        ));
    }
    if grant.kind.is_workspace()
        && body
            .channel_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        return Err(ServerError::bad_request_kind(
            "channel_id_required",
            "a workspace grant names the channel that will run this session",
        ));
    }
    // Resolve the durable conversation before inspecting mutable selectors.
    // A retry cannot clone a different repository or change its original channel.
    if let Some(binding) = tidebreak_core::db::code::get_external_binding(
        &runtime.db,
        &grant.owner,
        &grant.channel_kind,
        &body.external_key,
    )
    .await?
    {
        if binding.grant_id != grant.id {
            return Err(ServerError::not_found("code session not found"));
        }
        let session = runtime
            .get_session(&grant.owner, binding.session_id)
            .await?;
        let ended = session.lifecycle == tidebreak_core::SessionLifecycle::Ended;
        if !ended {
            repair_original_context(&runtime, &grant, &binding, body.channel_id.as_deref()).await?;
        }
        return Ok((
            StatusCode::OK,
            Json(ExternalSessionResponse {
                harness: session.harness_kind,
                model: session.model.clone(),
                settings_path: tidebreak_core::db::code::session_context(
                    &runtime.db,
                    &grant.owner,
                    session.id,
                )
                .await?
                .and_then(|context| context.channel_id)
                .map(|channel| crate::code::channel_preferences::settings_path(&grant, &channel)),
                status: if ended { "ended" } else { "existing" },
                session_id: binding.session_id,
                binding_id: (!ended).then_some(binding.id),
                acts_as: session.acts_as(),
                acting_login: None,
                app_name: None,
                connect_url: None,
            }),
        ));
    }
    let preferences = match body.channel_id.as_deref() {
        Some(channel) if grant.channel_kind == "slack" => {
            crate::code::channel_preferences::read(&runtime.db, &grant, channel).await?
        }
        _ => Default::default(),
    };
    let registered = match body.repo_id {
        Some(id) => Some(runtime.get_repo(&grant.owner, id).await?),
        None => None,
    };
    let repository = match registered.as_ref() {
        Some(repo) if grant.kind.is_workspace() => Some(workspace_repository_origin(repo)?),
        Some(repo) => match (repo.origin_owner.as_deref(), repo.origin_name.as_deref()) {
            (Some(owner), Some(name)) => Some(format!("{owner}/{name}")),
            _ => body.repository.clone(),
        },
        None => body
            .repository
            .as_deref()
            .map(tidebreak_server_core::code::runtime::CodeRuntime::canonical_external_repository)
            .transpose()?,
    };
    let owner_kind = state
        .principal_authenticator
        .session_owner_kind_for(&grant.owner);
    let repo_id = match (registered, repository.as_deref()) {
        (Some(repo), _) => Some(repo.id),
        (None, None) => None,
        (None, Some(origin)) => {
            let attribution = if grant.kind.is_workspace()
                || owner_kind == Some("service")
                || body.acts_as == Some(tidebreak_core::ActsAs::Bot)
            {
                tidebreak_server_core::obo_gateway::GitForgeAttributionRequest::Installation
            } else {
                tidebreak_server_core::obo_gateway::GitForgeAttributionRequest::Person
            };
            Some(
                runtime
                    .prepare_external_repository(&grant.owner, grant.id, origin, attribution)
                    .await?
                    .id,
            )
        }
    };
    // Explicit choices retain their placement, including machine-side Internal.
    // Only omitted choices consult the admitted default of the actual runtime.
    let harness = match body.harness.or(preferences.harness) {
        Some(harness) => harness,
        None if repo_id.is_none() && grant.channel_kind == "slack" => runtime
            .default_channel_sandbox_harness(&grant.owner)?
            .unwrap_or(HarnessKind::Internal),
        None if repo_id.is_none() => HarnessKind::Internal,
        None => HarnessKind::ClaudeCode,
    };
    let model =
        resolve_external_harness_model(&state, &grant, harness, preferences.model.as_deref())
            .await?;
    let (resolution, identity) = runtime
        .external_get_or_create_with_channel_context(
            &grant.owner,
            owner_kind,
            grant.id,
            &grant.channel_kind,
            &body.external_key,
            repo_id,
            body.title,
            harness,
            NewSessionSettings {
                permission_mode: PermissionMode::Allow,
                model,
                reasoning_effort: None,
                fast_mode: false,
                permission_mode_ceiling: None,
                acts_as: None,
            },
            body.permission_mode,
            body.acts_as,
            (grant.channel_kind == "slack").then_some(
                tidebreak_core::db::code::ExternalSessionChannelContext {
                    channel_id: body.channel_id.as_deref(),
                    instructions: &preferences.instructions,
                },
            ),
        )
        .await?;
    if let ExternalSessionResolution::Created(binding)
    | ExternalSessionResolution::Existing(binding) = &resolution
    {
        repair_original_context(&runtime, &grant, binding, body.channel_id.as_deref()).await?;
    }
    let resolved_session_id = match &resolution {
        ExternalSessionResolution::Created(binding)
        | ExternalSessionResolution::Existing(binding) => binding.session_id,
        ExternalSessionResolution::Ended { session_id } => *session_id,
        ExternalSessionResolution::GrantMismatch => {
            return Err(ServerError::not_found("code session not found"))
        }
    };
    let actual = runtime
        .get_session(&grant.owner, resolved_session_id)
        .await?;
    let response_from =
        |status: &'static str, session_id: SessionId, binding_id| ExternalSessionResponse {
            harness: actual.harness_kind,
            model: actual.model.clone(),
            settings_path: body
                .channel_id
                .as_deref()
                .map(|channel| crate::code::channel_preferences::settings_path(&grant, channel)),
            status,
            session_id,
            binding_id,
            acts_as: identity.acts_as,
            acting_login: identity.acting_login.clone(),
            app_name: identity.app_name.clone(),
            connect_url: identity.connect_url.clone(),
        };
    let (status, response) = match resolution {
        ExternalSessionResolution::Created(binding) => (
            StatusCode::CREATED,
            response_from("created", binding.session_id, Some(binding.id)),
        ),
        ExternalSessionResolution::Existing(binding) => (
            StatusCode::OK,
            response_from("existing", binding.session_id, Some(binding.id)),
        ),
        ExternalSessionResolution::Ended { session_id } => {
            (StatusCode::OK, response_from("ended", session_id, None))
        }
        ExternalSessionResolution::GrantMismatch => {
            return Err(ServerError::not_found("code session not found"));
        }
    };
    Ok((status, Json(response)))
}

#[derive(serde::Deserialize)]
pub struct ExternalBindingBody {
    pub external_key: String,
    #[serde(default)]
    pub channel_id: Option<String>,
}

/// Attach a conversation to the session this grant already holds.
pub async fn external_attach_binding(
    State(state): State<AppState>,
    ExternalGrantAuth(grant): ExternalGrantAuth,
    Path(id): Path<SessionId>,
    Json(body): Json<ExternalBindingBody>,
) -> Result<(StatusCode, Json<tidebreak_core::CodeExternalBinding>), ServerError> {
    let runtime = require_bound(&state, &grant, id).await?;
    let key = body.external_key.trim();
    if key.is_empty() || key.len() > 1024 || key.chars().any(char::is_control) {
        return Err(ServerError::bad_request_kind(
            "invalid_external_key",
            "a conversation key needs 1 to 1024 bytes and no control characters",
        ));
    }
    if grant.kind.is_workspace() {
        require_binding_repository(&runtime, &grant, id, body.channel_id.as_deref()).await?;
    }
    match tidebreak_core::db::code::attach_external_binding(
        &runtime.db,
        &grant.owner,
        grant.id,
        &grant.channel_kind,
        key,
        id,
    )
    .await?
    {
        ExternalSessionResolution::Created(binding) => Ok((StatusCode::CREATED, Json(*binding))),
        ExternalSessionResolution::Existing(binding) => Ok((StatusCode::OK, Json(*binding))),
        ExternalSessionResolution::Ended { .. } => Err(ServerError::conflict_kind(
            "ended",
            "the session has ended; start another session to continue",
        )),
        ExternalSessionResolution::GrantMismatch => {
            Err(ServerError::not_found("code session not found"))
        }
    }
}

fn workspace_repository_origin(repo: &tidebreak_core::CodeRepo) -> Result<String, ServerError> {
    tidebreak_server_core::code::runtime::CodeRuntime::workspace_repository_origin(repo)
}

async fn require_binding_repository(
    runtime: &crate::code::runtime::CodeRuntime,
    grant: &CodeExternalGrant,
    id: SessionId,
    channel_id: Option<&str>,
) -> Result<(), ServerError> {
    channel_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ServerError::bad_request_kind(
                "channel_id_required",
                "a workspace grant names the channel that will continue this session",
            )
        })?;
    let session = runtime.get_session(&grant.owner, id).await?;
    if let Some(workspace_id) = session.workspace_id {
        let workspace = runtime.get_workspace(&grant.owner, workspace_id).await?;
        let repo = runtime.get_repo(&grant.owner, workspace.repo_id).await?;
        let repository = workspace_repository_origin(&repo)?;
        runtime
            .require_workspace_repository_access(&grant.owner, grant.id, &repository)
            .await?;
    }
    Ok(())
}

/// List every conversation attached under the authenticated grant.
pub async fn external_bindings(
    State(state): State<AppState>,
    ExternalGrantAuth(grant): ExternalGrantAuth,
    Path(id): Path<SessionId>,
) -> Result<Json<Vec<tidebreak_core::CodeExternalBinding>>, ServerError> {
    let runtime = require_bound(&state, &grant, id).await?;
    let bindings =
        tidebreak_core::db::code::list_bindings_for_session(&runtime.db, &grant.owner, id).await?;
    Ok(Json(
        bindings
            .into_iter()
            .filter(|binding| binding.grant_id == grant.id)
            .collect(),
    ))
}

#[derive(serde::Deserialize)]
pub struct ExternalMessageBody {
    pub text: String,
    /// The channel's delivery id; replays of it answer from the first row.
    pub event_id: String,
    /// The channel's ordering token; still-queued messages apply in its
    /// order.
    pub channel_ts: String,
    /// Display name the channel supplied, when available.
    pub display: Option<String>,
    /// Required under a workspace grant: the person who sent the message.
    #[serde(default)]
    pub actor: Option<ExternalActor>,
    /// Prior thread messages, accepted on the first message only.
    #[serde(default)]
    pub context: Option<Vec<tidebreak_core::code::ExternalContextMessage>>,
    /// The channel explicitly enabled quoted thread context.
    #[serde(default)]
    pub context_opt_in: bool,
    /// The binding whose channel opted in.
    #[serde(default)]
    pub context_binding_id: Option<tidebreak_core::CodeBindingId>,
}

#[derive(serde::Deserialize)]
pub struct ExternalActor {
    pub external_identity: String,
    pub display: String,
}

#[derive(serde::Serialize)]
pub struct ExternalMessageResponse {
    /// `new_turn`, `queued`, or `dropped`.
    pub outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<tidebreak_core::TurnId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued: Option<QueuedTurn>,
}

/// `POST /external/code/sessions/{id}/messages` — deliver one message.
/// Idempotent on `event_id`; queue-default when the session is busy.
pub async fn external_messages(
    State(state): State<AppState>,
    ExternalGrantAuth(grant): ExternalGrantAuth,
    Path(id): Path<SessionId>,
    Json(body): Json<ExternalMessageBody>,
) -> Result<Json<ExternalMessageResponse>, ServerError> {
    let runtime = require_bound(&state, &grant, id).await?;
    if grant.kind.is_workspace() {
        if body.actor.is_none() {
            return Err(ServerError::bad_request_kind(
                "actor_required",
                "a workspace grant names the person who sent this message",
            ));
        }
    } else if body.actor.is_some() {
        return Err(ServerError::bad_request_kind(
            "actor_not_allowed",
            "a person grant takes the actor from the linked identity, not the body",
        ));
    }
    let context = match body.context {
        Some(messages) => {
            if !grant.kind.is_workspace() || !body.context_opt_in {
                return Err(ServerError::bad_request_kind(
                    "context_not_allowed",
                    "Thread context requires a workspace grant and explicit channel opt-in.",
                ));
            }
            let binding_id = body.context_binding_id.ok_or_else(|| {
                ServerError::bad_request_kind(
                    "context_binding_required",
                    "Thread context must name the channel binding that opted in.",
                )
            })?;
            Some(tidebreak_core::code::ExternalThreadContext {
                binding_id,
                grant_id: grant.id,
                messages,
            })
        }
        None => None,
    };
    let (external_identity, display) = if let Some(actor) = body.actor {
        (Some(actor.external_identity), Some(actor.display))
    } else {
        (Some(grant.external_identity.clone()), body.display)
    };
    // The turn is attributed to the channel identity, not to the shared
    // principal that owns the session (decision 0086).
    let outcome = runtime
        .external_submit_message(
            &grant.owner,
            grant.id,
            id,
            ExternalMessage {
                context,
                text: body.text,
                event_id: body.event_id,
                channel_ts: body.channel_ts,
                actor: tidebreak_core::TurnActor {
                    principal: None,
                    display,
                    channel_kind: Some(grant.channel_kind.clone()),
                    external_identity,
                },
            },
        )
        .await?;
    let response = match outcome {
        ExternalMessageOutcome::NewTurn(turn) => ExternalMessageResponse {
            outcome: "new_turn",
            turn_id: Some(turn.id),
            queued: None,
        },
        ExternalMessageOutcome::Queued(row) => ExternalMessageResponse {
            outcome: "queued",
            turn_id: Some(row.id),
            queued: Some(QueuedTurn::from(*row)),
        },
        ExternalMessageOutcome::Dropped => ExternalMessageResponse {
            outcome: "dropped",
            turn_id: None,
            queued: None,
        },
    };
    Ok(Json(response))
}

#[derive(serde::Deserialize)]
pub struct ExternalAccessBody {
    pub contributors: Vec<ExternalContributor>,
}

#[derive(serde::Deserialize)]
pub struct ExternalContributor {
    pub external_identity: String,
}

/// `PUT /external/code/sessions/{id}/access` — workspace grants only.
pub async fn external_session_access(
    State(state): State<AppState>,
    ExternalGrantAuth(grant): ExternalGrantAuth,
    Path(id): Path<SessionId>,
    Json(body): Json<ExternalAccessBody>,
) -> Result<StatusCode, ServerError> {
    if !grant.kind.is_workspace() {
        return Err(ServerError::not_found("code session not found"));
    }
    let runtime = require_bound(&state, &grant, id).await?;
    let identities: Vec<String> = body
        .contributors
        .into_iter()
        .map(|row| row.external_identity)
        .collect();
    tidebreak_core::db::code::replace_external_session_contributors(
        &runtime.db,
        &grant.owner,
        id,
        &grant.channel_kind,
        &identities,
        chrono::Utc::now(),
    )
    .await?
    .ok_or_else(|| ServerError::not_found("code session not found"))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Resolve the saved session selection without consulting channel or chat defaults.
async fn external_session_model_display_name(
    state: &AppState,
    grant: &CodeExternalGrant,
    session: &tidebreak_core::Session,
) -> Option<String> {
    let selection = session.model.as_deref()?;
    if !session.harness_kind.is_in_process() {
        return Some(selection.to_owned());
    }
    let runtime = state.code.as_ref()?;
    let snapshot = if let Some(relay) = runtime.harness_llm() {
        let result = if relay.external_delegations().is_some() {
            relay.catalog_for_grant(&grant.owner, grant.id).await
        } else {
            relay.catalog(&grant.owner).await
        };
        // A label lookup must not prevent the adapter from reading session state.
        match result {
            Ok(snapshot) => snapshot,
            Err(_) => return None,
        }
    } else {
        None
    };
    crate::providers::resolve_model_policy(&*state.store, selection, true, snapshot.as_ref())
        .await
        .ok()
        .flatten()
        .map(|policy| policy.display_name)
}

/// `WS /external/code/sessions/{id}/events?after=` — the desktop event
/// stream, scoped by grant, prefixed with a session snapshot (lifecycle
/// and attention), and severed the moment the grant is revoked.
pub async fn external_events(
    State(state): State<AppState>,
    ExternalGrantAuth(grant): ExternalGrantAuth,
    Path(id): Path<SessionId>,
    Query(query): Query<SessionEventsQuery>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ServerError> {
    let runtime = require_bound(&state, &grant, id).await?;
    let session = runtime.get_session(&grant.owner, id).await?;
    let bindings =
        tidebreak_core::db::code::list_bindings_for_session(&runtime.db, &grant.owner, id).await?;
    let model_display_name = external_session_model_display_name(&state, &grant, &session).await;
    let mut session_snapshot = SessionSnapshot::from(session);
    session_snapshot.set_external_origins(
        bindings
            .into_iter()
            .filter(|binding| binding.grant_id == grant.id),
    );
    let owner = grant.owner.clone();
    let grant_id = grant.id;
    // Subscribe before deciding to serve, then re-read the durable row while
    // holding the subscription. A revocation that committed before this point
    // published into a channel nobody held; the recheck catches it, and one
    // that commits after it reaches the subscription. Without this ordering,
    // a revocation racing the handshake leaves the stream connected.
    let revocations = runtime.grant_revocations();
    let mut severed = revocations.subscribe();
    if !runtime.adapter_grant_is_live(&owner, grant_id).await? {
        return Err(ServerError::unauthorized(
            "the adapter token matches no live grant",
        ));
    }
    Ok(upgrade.on_upgrade(move |mut socket| async move {
        // The renderer needs the session's standing before the journal:
        // lifecycle and the attention snapshot arrive first, as their own
        // frame shape.
        let mut snapshot = serde_json::json!({
            "snapshot": session_snapshot,
        });
        snapshot["snapshot"]["model_display_name"] = serde_json::json!(model_display_name);
        if let Ok(json) = serde_json::to_string(&snapshot) {
            if socket
                .send(axum::extract::ws::Message::Text(json.into()))
                .await
                .is_err()
            {
                return;
            }
        }
        let stream = super::session_events::stream_events(
            socket,
            state,
            owner,
            id,
            query.after,
            None,
            super::session_events::Viewer::Adapter,
        );
        tokio::pin!(stream);
        loop {
            tokio::select! {
                () = &mut stream => break,
                hit = severed.recv() => match hit {
                    // Dropping the stream future drops the socket, which
                    // closes the connection: revocation severs live.
                    Ok(revoked) if revoked == grant_id => break,
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        stream.await;
                        break;
                    }
                },
            }
        }
    }))
}

/// `POST /external/code/sessions/{id}/interrupt` — stop the active turn,
/// exactly as the desktop button does. The session stays live.
pub async fn external_interrupt(
    State(state): State<AppState>,
    ExternalGrantAuth(grant): ExternalGrantAuth,
    Path(id): Path<SessionId>,
) -> Result<StatusCode, ServerError> {
    let runtime = require_bound(&state, &grant, id).await?;
    let _ = runtime.get_session(&grant.owner, id).await?;
    runtime.interrupt(id).await?;
    Ok(StatusCode::ACCEPTED)
}

/// `POST /external/code/sessions/{id}/reap` — clear a fenced session,
/// exactly as the desktop button does.
pub async fn external_reap(
    State(state): State<AppState>,
    ExternalGrantAuth(grant): ExternalGrantAuth,
    Path(id): Path<SessionId>,
) -> Result<Json<SessionSnapshot>, ServerError> {
    let runtime = require_bound(&state, &grant, id).await?;
    let session = runtime.reap(&grant.owner, id).await?;
    let bindings =
        tidebreak_core::db::code::list_bindings_for_session(&runtime.db, &grant.owner, id).await?;
    let mut snapshot = SessionSnapshot::from(session);
    snapshot.set_external_origins(
        bindings
            .into_iter()
            .filter(|binding| binding.grant_id == grant.id),
    );
    Ok(Json(snapshot))
}

#[derive(serde::Deserialize)]
pub struct ExternalRotateBody {
    pub refresh: String,
}

#[derive(serde::Serialize)]
pub struct ExternalRotateResponse {
    pub token: String,
    pub refresh: String,
}

/// `POST /external/grants/rotate` — trade the refresh token for a new
/// pair. A replayed rotated token revokes the grant before this answers,
/// and the answer says so.
pub async fn external_rotate(
    State(state): State<AppState>,
    Json(body): Json<ExternalRotateBody>,
) -> Result<Json<ExternalRotateResponse>, ServerError> {
    let runtime = state
        .code
        .clone()
        .ok_or_else(|| ServerError::unauthorized("adapter access is not configured"))?;
    let (outcome, pair) = runtime.rotate_adapter_token(&body.refresh).await?;
    match outcome {
        GrantRotation::Rotated(_) => {
            let pair = pair.ok_or_else(|| ServerError::internal("rotation issued no pair"))?;
            Ok(Json(ExternalRotateResponse {
                token: pair.token,
                refresh: pair.refresh,
            }))
        }
        GrantRotation::ReuseDetected(_) => Err(ServerError::unauthorized(
            "this refresh token was already rotated; the grant is revoked",
        )),
        GrantRotation::Unknown => Err(ServerError::unauthorized(
            "the refresh token matches no live grant",
        )),
    }
}

#[derive(serde::Deserialize)]
pub struct ExternalDecisionActor {
    pub external_identity: String,
    #[serde(default)]
    pub display: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct ExternalDecisionBody {
    pub decision: ApprovalDecision,
    #[serde(default)]
    pub feedback: Option<String>,
    /// Workspace-grant messages name the person in the body. A person grant
    /// omits this and the machine records the grant identity.
    #[serde(default)]
    pub actor: Option<ExternalDecisionActor>,
}

/// Read the complete approval after verifying its session and adapter grant.
/// The journal can then carry a bounded reference while cards load questions
/// and plan text from the approval row.
pub async fn external_approval(
    State(state): State<AppState>,
    ExternalGrantAuth(grant): ExternalGrantAuth,
    Path((id, call)): Path<(SessionId, ApprovalId)>,
) -> Result<Json<ApprovalSnapshot>, ServerError> {
    let runtime = require_bound(&state, &grant, id).await?;
    let approval = runtime.get_approval(&grant.owner, call).await?;
    if approval.session_id != id {
        return Err(ServerError::not_found("code session not found"));
    }
    Ok(Json(ApprovalSnapshot::from(approval)))
}

/// `POST /external/code/sessions/{id}/approvals/{call}/decision`
pub async fn external_approval_decision(
    State(state): State<AppState>,
    ExternalGrantAuth(grant): ExternalGrantAuth,
    Path((id, call)): Path<(SessionId, ApprovalId)>,
    Json(body): Json<ExternalDecisionBody>,
) -> Result<Json<ApprovalSnapshot>, ServerError> {
    external_decide(state, grant, id, call, body).await
}

/// `POST /external/code/sessions/{id}/questions/{call}/decision`
pub async fn external_question_decision(
    State(state): State<AppState>,
    ExternalGrantAuth(grant): ExternalGrantAuth,
    Path((id, call)): Path<(SessionId, ApprovalId)>,
    Json(body): Json<ExternalDecisionBody>,
) -> Result<Json<ApprovalSnapshot>, ServerError> {
    external_decide(state, grant, id, call, body).await
}

/// `POST /external/code/sessions/{id}/plans/{call}/decision`
pub async fn external_plan_decision(
    State(state): State<AppState>,
    ExternalGrantAuth(grant): ExternalGrantAuth,
    Path((id, call)): Path<(SessionId, ApprovalId)>,
    Json(body): Json<ExternalDecisionBody>,
) -> Result<Json<ApprovalSnapshot>, ServerError> {
    external_decide(state, grant, id, call, body).await
}

async fn external_decide(
    state: AppState,
    grant: CodeExternalGrant,
    session_id: SessionId,
    call: ApprovalId,
    body: ExternalDecisionBody,
) -> Result<Json<ApprovalSnapshot>, ServerError> {
    let runtime = require_bound(&state, &grant, session_id).await?;
    let approval = runtime.get_approval(&grant.owner, call).await?;
    if approval.session_id != session_id {
        return Err(ServerError::not_found("code session not found"));
    }
    let decision = match body.decision {
        ApprovalDecision::Approve => ApprovalDecisionRequest::Approve,
        ApprovalDecision::Deny => ApprovalDecisionRequest::Deny {
            feedback: body.feedback,
        },
        ApprovalDecision::ApproveWithGrant { grant_index } => {
            ApprovalDecisionRequest::ApproveWithGrant { grant_index }
        }
        ApprovalDecision::Answers { answers } => ApprovalDecisionRequest::Answers { answers },
        ApprovalDecision::PlanDecision { approve } => ApprovalDecisionRequest::PlanDecision {
            approve,
            feedback: body.feedback,
        },
    };
    let actor = match body.actor {
        Some(actor) => tidebreak_core::TurnActor {
            principal: None,
            display: actor.display,
            channel_kind: Some(grant.channel_kind.clone()),
            external_identity: Some(actor.external_identity),
        },
        None => tidebreak_core::TurnActor {
            principal: None,
            display: None,
            channel_kind: Some(grant.channel_kind.clone()),
            external_identity: Some(grant.external_identity.clone()),
        },
    };
    let settled = runtime
        .decide_approval(&grant.owner, call, decision, Some(actor))
        .await?;
    Ok(Json(ApprovalSnapshot::from(settled)))
}

#[cfg(test)]
mod harness_model_tests {
    use super::*;

    fn listing(id: &str) -> tidebreak_core::Result<Vec<crate::obo_gateway::GatewayCompatModel>> {
        Ok(vec![crate::obo_gateway::GatewayCompatModel {
            id: id.to_owned(),
            display_name: None,
            family_default: false,
        }])
    }

    #[test]
    fn external_compat_models_preserve_harness_namespace_and_protocol() {
        for (harness, selected) in [
            (HarnessKind::ClaudeCode, "anthropic-alias"),
            (HarnessKind::Codex, "openai-alias"),
            (HarnessKind::Grok, "openai-alias"),
            (HarnessKind::Opencode, "anthropic/anthropic-alias"),
            (HarnessKind::Opencode, "model-gateway/openai-alias"),
        ] {
            assert_eq!(
                select_external_compat_model(
                    harness,
                    selected,
                    listing("anthropic-alias"),
                    listing("openai-alias")
                )
                .unwrap(),
                selected
            );
        }
        for (harness, selected) in [
            (HarnessKind::ClaudeCode, "openai-alias"),
            (HarnessKind::Codex, "anthropic-alias"),
            (HarnessKind::Opencode, "openai-alias"),
            (HarnessKind::Opencode, "anthropic/openai-alias"),
            (
                HarnessKind::Codex,
                "model_gateway::__tidebreak_gateway_v1.route",
            ),
        ] {
            assert!(select_external_compat_model(
                harness,
                selected,
                listing("anthropic-alias"),
                listing("openai-alias")
            )
            .is_err());
        }
        assert_eq!(
            select_external_compat_model(
                HarnessKind::ClaudeCode,
                "anthropic-alias",
                listing("anthropic-alias"),
                Err(tidebreak_core::AgentError::msg("unrelated listing down"))
            )
            .unwrap(),
            "anthropic-alias"
        );
    }
}
