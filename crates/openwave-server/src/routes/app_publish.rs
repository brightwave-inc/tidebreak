//! Publishing a gateway-bound local app to a team.
//!
//! An app the author built here is theirs alone until they say otherwise.
//! Publishing is that saying: the gateway takes the app's current revision and
//! makes it reachable to one team the author belongs to, and from then on the
//! gateway — not this host — decides who may open it and what it may call as
//! them.
//!
//! Both handlers serve one flow, which is why they live together despite
//! addressing different resources. `GET /gateway/teams` is what the affordance
//! is *for*: with no gateway, or a deployment that cannot publish, it answers
//! `supported: false` and the renderer shows nothing rather than offering a
//! button that can only fail. `POST /apps/{id}/publish` runs the act itself.
//!
//! Every way a publish can fail to happen is an answer here, not an error:
//! the author asked a legitimate question, and "your gateway said no, in these
//! words" is the reply. Only a genuinely broken local request — no such app,
//! an app that binds nothing at the gateway — is a `ServerError`.

use axum::extract::State;
use serde::{Deserialize, Serialize};

use openwave_core::id::AppId;

use crate::connected_apps::GatewayPublish;
use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::state::AppState;

/// The longest team id a publish body may carry. The id is the gateway's and
/// is never interpreted here; this only keeps an absurd body from being
/// relayed onward.
const MAX_TEAM_ID_BYTES: usize = 128;

/// `GET /gateway/teams` — the teams a publish may name.
pub async fn get_gateway_teams(
    State(state): State<AppState>,
) -> Result<Json<crate::gateway_runtime::GatewayTeams>, ServerError> {
    Ok(Json(state.gateway.teams().await?))
}

/// The publish body: which team the app is being published to.
#[derive(Debug, Deserialize, ts_rs::TS)]
pub struct AppPublishRequest {
    /// The gateway's own team id, from `GET /gateway/teams`.
    pub team_id: String,
}

/// What one publish attempt came back as.
///
/// A closed union rather than prose, because the renderer branches on it: only
/// `refused` and `app_disabled` carry words worth showing verbatim, and only
/// `published` is a success. `message` is the gateway's own wherever it has
/// one — a bundle refused for calling host-local bridge verbs names those
/// verbs, and no wording assembled here could.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum AppPublishOutcome {
    /// The gateway published this app's current revision to the team.
    Published,
    /// This profile has no model gateway to publish to.
    NoGateway,
    /// The app could not be registered at the gateway, so there was nothing
    /// to publish. The next attempt registers and publishes in one go.
    NotRegistered,
    /// The gateway did not accept the publish at all: a deployment that
    /// predates publishing, or an app or team no longer this author's.
    NotSupported,
    /// The shared app is switched off at the gateway.
    AppDisabled,
    /// The gateway refused, in the words carried by `message`.
    Refused,
    /// The gateway could not be reached, or answered something this host
    /// could not read.
    Unreachable,
}

/// The publish answer: one outcome, plus whatever words came with it.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct AppPublishResult {
    pub outcome: AppPublishOutcome,
    /// The gateway's own message, when the outcome carries one.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message: Option<String>,
    /// The gateway's own refusal code, when it named one — so a renderer can
    /// tell a bundle it must change from a team it may not publish to without
    /// matching on wording.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub code: Option<String>,
}

impl AppPublishResult {
    fn plain(outcome: AppPublishOutcome) -> Self {
        Self {
            outcome,
            message: None,
            code: None,
        }
    }
}

/// `POST /apps/{id}/publish` — publish one app's current revision to a team.
///
/// Deliberately not gated on the local grant. A grant is this machine's
/// consent to *run* the app here; publishing is the author's decision about
/// their own work, and the gateway runs its own consent gate for every viewer
/// who opens it there. Requiring a local grant would only mean an author had
/// to open their app before they could share it.
pub async fn post_app_publish(
    State(state): State<AppState>,
    Path(app_id): Path<AppId>,
    Json(body): Json<AppPublishRequest>,
) -> Result<Json<AppPublishResult>, ServerError> {
    let team_id = body.team_id.trim();
    if team_id.is_empty() || team_id.len() > MAX_TEAM_ID_BYTES {
        return Err(ServerError::bad_request("a team id is required"));
    }
    let (app, revision) = super::app_grant::current_live_app(&state, app_id).await?;
    // An app that binds nothing at the gateway has nothing there to be. It
    // would register as an empty shared app and publish a shell no viewer
    // could do anything with, so it is refused here rather than half-shared.
    if crate::connected_apps::gateway_apps_bound_by(&revision.manifest.bindings).is_empty() {
        return Err(ServerError::conflict_kind(
            "app_not_gateway_bound",
            format!(
                "{:?} uses nothing from your model gateway, so there is nothing to \
                 publish there",
                app.name
            ),
        ));
    }
    // The same gating the registration path uses: with no managed gateway
    // there is no deployment a publish could name.
    let Some(base_url) = crate::gateway_drafts::registration_base_url(&state.gateway).await else {
        return Ok(Json(AppPublishResult::plain(AppPublishOutcome::NoGateway)));
    };
    let published = state
        .gateway_drafts
        .publish(app.id, &base_url, team_id)
        .await;
    Ok(Json(match published {
        Ok(GatewayPublish::Published) => AppPublishResult::plain(AppPublishOutcome::Published),
        Ok(GatewayPublish::NotRegistered) => {
            AppPublishResult::plain(AppPublishOutcome::NotRegistered)
        }
        Ok(GatewayPublish::NotSupported) => {
            AppPublishResult::plain(AppPublishOutcome::NotSupported)
        }
        Ok(GatewayPublish::AppDisabled { message }) => AppPublishResult {
            outcome: AppPublishOutcome::AppDisabled,
            message: Some(message),
            code: None,
        },
        Ok(GatewayPublish::Refused { code, message }) => AppPublishResult {
            outcome: AppPublishOutcome::Refused,
            message: Some(message),
            code,
        },
        // A publish that could not happen at all. The client's own errors are
        // host-authored and carry no URL or credential material, so they are
        // shown: "could not reach your gateway" alone leaves an author with
        // nothing to act on.
        Err(error) => {
            tracing::warn!(%app_id, "could not publish this app to a team: {error}");
            AppPublishResult {
                outcome: AppPublishOutcome::Unreachable,
                message: Some(error.to_string()),
                code: None,
            }
        }
    }))
}
