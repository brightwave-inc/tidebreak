//! Where a gateway-bound local app lives at its gateway.
//!
//! Publishing a shared app — and re-publishing a later revision — is a
//! governance action, done on the gateway's own web surface alongside the
//! publish state, the team grants, and the revocation that belong with it.
//! Decision record 11 states why: publishing mutates gateway-owned entitlement
//! state, every other mutation of that state is already done at the gateway,
//! and a second interface of record in each harness that authors apps can only
//! drift from the first. So this host offers the way *there* and nothing else.
//!
//! Registration happens on the way, which is the one thing worth explaining. A
//! draft is registered lazily — on the first relay or consent — so an app the
//! author has only ever run locally has no page at the gateway yet, and a link
//! to one would be a link to nothing. Registering here is not publishing: it
//! creates the author's own draft, reachable by them alone, and changes
//! nothing about who else can open the app.
//!
//! Every way this can fail to produce an address is an answer rather than an
//! error, for the same reason the invoke ladder answers: the author asked a
//! legitimate question, and "your gateway would not hold this, in these words"
//! is the reply. Only a broken local request — no such app, or an app that
//! binds nothing at the gateway — is a `ServerError`.

use axum::extract::State;
use serde::Serialize;

use openwave_core::id::AppId;

use crate::connected_apps::GatewayRegistration;
use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::state::AppState;

/// What asking for an app's gateway page came back as.
///
/// A closed union rather than prose, because the renderer branches on it: only
/// `ready` carries somewhere to go, and only `refused` and `unreachable` carry
/// words worth showing verbatim — a gateway that will not hold a bundle names
/// what about it, and no wording assembled here could.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum AppGatewayPageOutcome {
    /// `url` addresses this app's page at the gateway.
    Ready,
    /// This profile has no model gateway, so there is no page to open.
    NoGateway,
    /// Nothing at this deployment holds the app and nothing there can: the
    /// gateway predates shared-app registration.
    NotRegistered,
    /// The gateway would not register the app, in the words carried by
    /// `message`.
    Refused,
    /// The gateway could not be reached, or answered something this host
    /// could not read.
    Unreachable,
}

/// One answer: an outcome, and whatever came with it.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct AppGatewayPageResult {
    pub outcome: AppGatewayPageOutcome,
    /// The app's page at the gateway, present exactly when `outcome` is
    /// `ready`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
    /// The gateway's own message, when the outcome carries one.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message: Option<String>,
}

impl AppGatewayPageResult {
    fn plain(outcome: AppGatewayPageOutcome) -> Self {
        Self {
            outcome,
            url: None,
            message: None,
        }
    }
}

/// The gateway's own address for one shared app's page.
///
/// Built through `Url` rather than by formatting, so the id is percent-encoded
/// into exactly one path segment. Gateway identifiers are validated as
/// printable ASCII and nothing narrower, which admits `/` and `?`; formatted
/// into a string, one of those would silently address a different page at the
/// same origin.
///
/// `None` means the base URL does not parse or cannot take a path, which the
/// caller reports rather than papering over — a deployment addressed by
/// something that is not a URL has no page either.
fn shared_app_page_url(base_url: &str, shared_app_id: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(base_url).ok()?;
    url.path_segments_mut()
        .ok()?
        // The stored base ends in a slash, which is an empty trailing segment;
        // left in place it would address `//shared-apps/...`.
        .pop_if_empty()
        .extend(["shared-apps", shared_app_id]);
    Some(url.to_string())
}

/// `POST /apps/{id}/gateway-page` — where to send an author who wants to
/// share this app.
///
/// A `POST` because it is not a read: an app that has never been registered is
/// registered here, so that the address handed back is one the gateway will
/// actually serve.
pub async fn post_app_gateway_page(
    State(state): State<AppState>,
    Path(app_id): Path<AppId>,
) -> Result<Json<AppGatewayPageResult>, ServerError> {
    let (app, revision) = super::app_grant::current_live_app(&state, app_id).await?;
    // An app that binds nothing at the gateway has nothing there to be. It
    // would register as an empty shared app, so it is refused here rather than
    // sending the author to a page for a shell they cannot share.
    if crate::connected_apps::gateway_apps_bound_by(&revision.manifest.bindings).is_empty() {
        return Err(ServerError::conflict_kind(
            "app_not_gateway_bound",
            format!(
                "{:?} uses nothing from your model gateway, so it has no page there",
                app.name
            ),
        ));
    }
    // The same gating the registration path uses: with no managed gateway
    // there is no deployment whose page this could be.
    let Some(base_url) = crate::gateway_drafts::registration_base_url(&state.gateway).await else {
        return Ok(Json(AppGatewayPageResult::plain(
            AppGatewayPageOutcome::NoGateway,
        )));
    };
    let registered = state
        .gateway_drafts
        .ensure_registered(app.id, &base_url)
        .await;
    Ok(Json(match registered {
        Ok(GatewayRegistration::Registered { shared_app_id, .. }) => {
            let normalized = crate::gateway_drafts::normalized_gateway_base_url(&base_url);
            match shared_app_page_url(&normalized, &shared_app_id) {
                Some(url) => AppGatewayPageResult {
                    outcome: AppGatewayPageOutcome::Ready,
                    url: Some(url),
                    message: None,
                },
                None => AppGatewayPageResult {
                    outcome: AppGatewayPageOutcome::Unreachable,
                    url: None,
                    message: Some(format!(
                        "{base_url:?} is not an address this host can build a page link from"
                    )),
                },
            }
        }
        Ok(GatewayRegistration::NotRegistered) => {
            AppGatewayPageResult::plain(AppGatewayPageOutcome::NotRegistered)
        }
        Ok(GatewayRegistration::Refused { message }) => AppGatewayPageResult {
            outcome: AppGatewayPageOutcome::Refused,
            url: None,
            message: Some(message),
        },
        // The client's own errors are host-authored and carry no URL or
        // credential material, so they are shown: "could not reach your
        // gateway" alone leaves an author with nothing to act on.
        Err(error) => {
            tracing::warn!(%app_id, "could not resolve this app's page at the gateway: {error}");
            AppGatewayPageResult {
                outcome: AppGatewayPageOutcome::Unreachable,
                url: None,
                message: Some(error.to_string()),
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::shared_app_page_url;

    /// The id lands in one path segment, whatever it contains. A gateway
    /// identifier is only required to be printable ASCII, so an id carrying a
    /// separator must not be able to re-aim the link at another page — the one
    /// case where formatting the URL by hand would quietly do the wrong thing.
    #[test]
    fn an_id_is_one_segment_of_the_page_link() {
        assert_eq!(
            shared_app_page_url("https://gateway.example.com/", "abc-123").as_deref(),
            Some("https://gateway.example.com/shared-apps/abc-123")
        );
        assert_eq!(
            shared_app_page_url("https://gateway.example.com/", "../../admin").as_deref(),
            Some("https://gateway.example.com/shared-apps/..%2F..%2Fadmin")
        );
        assert_eq!(
            shared_app_page_url("https://gateway.example.com/", "a?b#c").as_deref(),
            Some("https://gateway.example.com/shared-apps/a%3Fb%23c")
        );
        assert_eq!(shared_app_page_url("not a url", "abc"), None);
    }
}
