//! OAuth sign-in for remote HTTP MCP servers, from the runtime's side.
//!
//! The connector (`crate::connectors`, `mcp_oauth`) speaks the protocol. This
//! module decides which servers can sign in, recognizes a server that asks
//! for it, keeps each sign-in's progress, and projects the status Settings
//! shows.
//!
//! A server does not have to be marked as OAuth. An HTTP server that sends no
//! static credential and answers the handshake with a `401` is asked how to
//! authorize: when its metadata names an authorization server, it needs a
//! sign-in, and the person connects it from Settings. The saved `oauth` flag
//! still forces that path, but nothing depends on it being set, so imported
//! and older definitions behave the same way.
//!
//! A stored session belongs to the one server URL it was issued for. It is
//! presented only to that URL, and the runtime clears it when the server's
//! URL changes or the server is removed.

use std::sync::Arc;
use std::time::Duration;

use tidebreak_core::id::ConnectedAppId;
use tidebreak_core::SecretProvider;

use crate::connectors::{
    ClientRegistration, Discovery, McpOAuthClient, McpOAuthCredentials, OAuthUnsupported,
};
use crate::mcp_oauth_runtime::{McpOAuthState, McpOAuthStatus};

use super::types::{McpServerDefinition, ReconnectPark};

/// How long the runtime waits for a server's `401` challenge when it asks the
/// server how to authorize.
pub(super) const CHALLENGE_TIMEOUT: Duration = Duration::from_secs(10);

/// The most asking a server how to authorize may add to a failed connection.
/// A server that takes longer keeps its `401`, and the next attempt asks again.
const DETECTION_TIMEOUT: Duration = Duration::from_secs(30);

/// The shared reason for an HTTP server that has no OAuth sign-in to run.
pub(super) const NO_OAUTH: &str = "This server does not ask for an OAuth sign-in.";

/// Why Connect could not start for a server known to sign in: its sign-in
/// metadata or its sign-in service did not answer this time.
pub(super) const METADATA_UNREADABLE: &str = "Tidebreak could not reach this server's sign-in \
                                              service. Check your connection, then select \
                                              Connect to try again.";

/// Why a finished sign-in stored nothing: the server's settings changed while
/// the browser had the page, so the session is for a server that is gone.
pub(super) const CHANGED_DURING_SIGN_IN: &str = "This server's settings changed while you were \
                                                 signing in, so the sign-in was not kept. \
                                                 Select Connect to sign in again.";

/// What a refused connection taught the runtime about a server's sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OAuthNeed {
    /// The server asks for an OAuth sign-in Tidebreak can run. The person
    /// signs in with Connect, on a page at `host`.
    SignIn { host: String },
    /// The server refused the static bearer token it is configured with, and
    /// its challenge names protected-resource metadata for an OAuth sign-in
    /// Tidebreak can run, on a page at `host`. Settings offers Use OAuth,
    /// which switches the server to OAuth.
    Offered { host: String },
    /// The server asks for an OAuth sign-in Tidebreak cannot complete.
    Unsupported(OAuthUnsupported),
    /// The server asks for an OAuth sign-in, but its sign-in service did not
    /// answer. Temporary: the supervisor keeps retrying on its usual backoff.
    Unavailable,
}

impl OAuthNeed {
    /// What Settings says about a server in this state. Never names a URL.
    pub(super) fn diagnostic(&self) -> String {
        match self {
            Self::SignIn { .. } => {
                "This server needs you to sign in. Select Connect to sign in with your browser."
                    .to_string()
            }
            Self::Offered { host } => format!(
                "This server did not accept the bearer token. It offers an OAuth sign-in on \
                 {host} instead: select Use OAuth to sign in with your browser, or correct the \
                 token."
            ),
            Self::Unsupported(reason) => format!(
                "This server asks you to sign in, but Tidebreak cannot complete its sign-in. {} \
                 If the server offers access tokens, set a bearer token variable instead.",
                reason.reason()
            ),
            Self::Unavailable => "This server asks you to sign in, but its sign-in service did \
                                  not answer. Tidebreak will try again."
                .to_string(),
        }
    }

    /// Why retrying cannot help until someone signs in or changes the server,
    /// if that is so. A sign-in service that did not answer may answer next
    /// time. A bearer the server refused stays refused until the settings
    /// change.
    pub(super) fn park(&self) -> Option<ReconnectPark> {
        match self {
            Self::SignIn { .. } | Self::Unsupported(_) => Some(ReconnectPark::Authorization),
            Self::Offered { .. } => Some(ReconnectPark::Configuration),
            Self::Unavailable => None,
        }
    }

    /// Whether retrying cannot help until someone signs in or changes the
    /// server.
    #[cfg(test)]
    pub(super) fn parks(&self) -> bool {
        self.park().is_some()
    }

    /// Whether Save and verify keeps the server. Only a saved server can be
    /// signed in to, so one that asks for a sign-in saves, and so does one
    /// that offers a sign-in in place of the bearer it refused. One whose
    /// sign-in Tidebreak cannot complete fails the save with the reason.
    pub(super) fn saves(&self) -> bool {
        !matches!(self, Self::Unsupported(_))
    }
}

/// Whether `definition` can sign in with OAuth: a server the person configured
/// that connects over HTTP, sends no static bearer, and has a URL that can
/// carry a credential (`https`, or `http` on a literal loopback address).
///
/// Gateway mounts carry their own session, and plugin servers carry the
/// headers their package declared, so neither signs in here.
pub(super) fn signs_in(definition: &McpServerDefinition) -> bool {
    definition.gateway_endpoint.is_none()
        && definition.plugin.is_none()
        && definition.launch.is_none()
        && definition.bearer_token_env.is_none()
        && !definition.bearer_token_stored
        && definition
            .url
            .as_deref()
            .is_some_and(|url| tidebreak_mcp::validate_http_url_with_credentials(url, true).is_ok())
}

/// Whether `definition` is a server the person configured with a static
/// bearer token, from a variable or the credential store, whose URL can carry
/// an OAuth token. When such a server refuses its bearer and names OAuth
/// metadata, Settings offers to switch it to OAuth.
pub(super) fn may_offer_sign_in(definition: &McpServerDefinition) -> bool {
    definition.gateway_endpoint.is_none()
        && definition.plugin.is_none()
        && definition.launch.is_none()
        && (definition.bearer_token_env.is_some() || definition.bearer_token_stored)
        && definition
            .url
            .as_deref()
            .is_some_and(|url| tidebreak_mcp::validate_http_url_with_credentials(url, true).is_ok())
}

/// The URL a definition signs in at, when it [signs in](signs_in).
pub(super) fn sign_in_url(definition: &McpServerDefinition) -> Option<&str> {
    signs_in(definition)
        .then_some(definition.url.as_deref())
        .flatten()
}

/// What a connection needs to present a stored OAuth session.
#[derive(Clone)]
pub(super) struct OAuthAccess {
    pub(super) secrets: Arc<dyn SecretProvider>,
    pub(super) id: ConnectedAppId,
    pub(super) client: McpOAuthClient,
}

/// Ask a server that refused an unauthenticated handshake how to authorize.
///
/// `None` means it publishes no OAuth metadata Tidebreak can find, so the
/// `401` stays an ordinary authentication failure.
pub(super) async fn detect(url: &str, client: &McpOAuthClient) -> Option<OAuthNeed> {
    let resource = url::Url::parse(url).ok()?;
    let ask = async {
        let challenge = tidebreak_mcp::authorization_challenge(url, CHALLENGE_TIMEOUT)
            .await
            .ok()
            .flatten();
        match client.discover(&resource, challenge.as_deref()).await? {
            Discovery::Supported(discovered) => Some(OAuthNeed::SignIn {
                host: discovered.sign_in_host,
            }),
            Discovery::Unsupported(reason) => Some(OAuthNeed::Unsupported(reason)),
            Discovery::Unavailable => Some(OAuthNeed::Unavailable),
        }
    };
    tokio::time::timeout(DETECTION_TIMEOUT, ask)
        .await
        .ok()
        .flatten()
}

/// Ask a server that refused its static bearer token whether it offers an
/// OAuth sign-in instead.
///
/// Only a `401` challenge that names protected-resource metadata (RFC 9728
/// §5.1) counts, and only when that metadata leads to a sign-in Tidebreak
/// can run. `None` leaves the `401` an ordinary authentication failure: the
/// token is wrong, and nothing else is on offer.
pub(super) async fn detect_offer(url: &str, client: &McpOAuthClient) -> Option<OAuthNeed> {
    let resource = url::Url::parse(url).ok()?;
    let ask = async {
        let challenge = tidebreak_mcp::authorization_challenge(url, CHALLENGE_TIMEOUT)
            .await
            .ok()
            .flatten()?;
        crate::connectors::resource_metadata_from_challenge(&challenge)?;
        match client.discover(&resource, Some(&challenge)).await? {
            Discovery::Supported(discovered) => Some(OAuthNeed::Offered {
                host: discovered.sign_in_host,
            }),
            Discovery::Unsupported(_) | Discovery::Unavailable => None,
        }
    };
    tokio::time::timeout(DETECTION_TIMEOUT, ask)
        .await
        .ok()
        .flatten()
}

/// The status Settings shows for a server that refused its static bearer and
/// offers an OAuth sign-in instead: `Available`, with the sign-in host. `None`
/// for any other need.
pub(super) fn offered_status(need: Option<&OAuthNeed>) -> Option<McpOAuthStatus> {
    match need {
        Some(OAuthNeed::Offered { host }) => {
            Some(McpOAuthStatus::of(McpOAuthState::Available).with_sign_in_host(Some(host.clone())))
        }
        _ => None,
    }
}

/// One server's sign-in, with the URL it signs in to. A record whose URL no
/// longer matches the server's is dropped, and a pending one stopped.
pub(super) struct SignInRecord {
    pub(super) server_url: String,
    pub(super) progress: SignInProgress,
}

/// One server's sign-in while it runs, and after it fails.
pub(super) enum SignInProgress {
    /// The person's browser has the authorization page; the loopback
    /// listener waits for its redirect.
    Pending {
        authorization_url: String,
        sign_in_host: String,
        /// Tells this sign-in apart from a newer one for the same server, so
        /// a superseded sign-in cannot record its outcome.
        generation: u64,
        task: tokio::task::AbortHandle,
    },
    /// The last sign-in stopped without storing a session. Shown until the
    /// next Connect, a Disconnect, or the server's removal.
    Failed {
        state: McpOAuthState,
        message: String,
    },
}

/// A sign-in's progress without its task handle, for projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SignInView {
    Pending { url: String, host: String },
    Failed(McpOAuthState, String),
}

impl SignInProgress {
    pub(super) fn view(&self) -> SignInView {
        match self {
            Self::Pending {
                authorization_url,
                sign_in_host,
                ..
            } => SignInView::Pending {
                url: authorization_url.clone(),
                host: sign_in_host.clone(),
            },
            Self::Failed { state, message } => SignInView::Failed(*state, message.clone()),
        }
    }
}

/// The sentence for a stored session the server no longer accepts.
const SESSION_REJECTED: &str =
    "Your sign-in is no longer valid. Select Reconnect to sign in again.";

/// The sentence for a stored session that expired with no way to refresh it.
const SESSION_EXPIRED: &str = "Your sign-in expired. Select Reconnect to sign in again.";

/// The sentence for a server whose sign-in service did not answer.
const SERVICE_DOWN: &str =
    "Its sign-in service did not answer. Tidebreak will try again, or you can select Connect.";

/// The OAuth status Settings shows for one server that [`signs_in`], or
/// `None` when nothing about the server involves OAuth.
///
/// `flag` is the saved `oauth` setting, `need` what the last connection
/// attempt learned, `progress` the last sign-in, and `stored` the session in
/// the credential store for this server's URL. A sign-in in flight wins. A
/// usable session the server has not refused reads as connected, even after a
/// later sign-in failed. Otherwise the last failure explains itself, then
/// what the server asked for, then what is stored. Each status carries the
/// sign-in host when one is known, so the row shows where Connect goes.
pub(super) fn project_status(
    flag: bool,
    need: Option<&OAuthNeed>,
    progress: Option<&SignInView>,
    stored: Option<&(ClientRegistration, McpOAuthCredentials)>,
) -> Option<McpOAuthStatus> {
    if let Some(SignInView::Pending { url, host }) = progress {
        return Some(
            McpOAuthStatus::authorizing(url.clone()).with_sign_in_host(Some(host.clone())),
        );
    }
    let stored_host = stored.and_then(|(registration, _)| registration.sign_in_host.clone());
    let need_host = match need {
        Some(OAuthNeed::SignIn { host }) => Some(host.clone()),
        _ => None,
    };
    let host = need_host.or(stored_host);
    let usable = stored.is_some_and(|(_, credentials)| {
        credentials.access_is_fresh() || credentials.refresh_token.is_some()
    });
    if usable && need.is_none() {
        return Some(McpOAuthStatus::connected().with_sign_in_host(host));
    }
    if let Some(SignInView::Failed(state, message)) = progress {
        return Some(McpOAuthStatus::failed(*state, message.clone()).with_sign_in_host(host));
    }
    let status = match need {
        Some(OAuthNeed::Offered { .. }) => return offered_status(need),
        Some(OAuthNeed::Unsupported(reason)) => {
            return Some(McpOAuthStatus::failed(
                McpOAuthState::Unsupported,
                reason.reason(),
            ));
        }
        Some(OAuthNeed::SignIn { .. } | OAuthNeed::Unavailable) if stored.is_some() => {
            McpOAuthStatus::failed(McpOAuthState::Expired, SESSION_REJECTED)
        }
        Some(OAuthNeed::SignIn { .. }) => McpOAuthStatus::not_connected(),
        Some(OAuthNeed::Unavailable) => {
            McpOAuthStatus::failed(McpOAuthState::NotConnected, SERVICE_DOWN)
        }
        None if stored.is_some() => McpOAuthStatus::failed(McpOAuthState::Expired, SESSION_EXPIRED),
        None if flag => McpOAuthStatus::not_connected(),
        None => return None,
    };
    Some(status.with_sign_in_host(host))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(fresh: bool, refresh: bool) -> (ClientRegistration, McpOAuthCredentials) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        (
            ClientRegistration {
                client_id: "client".to_string(),
                client_secret: None,
                registration_access_token: None,
                registration_client_uri: None,
                token_endpoint: Some("https://auth.example.test/token".to_string()),
                scopes: Vec::new(),
                resource: Some("https://mcp.example.test/".to_string()),
                server_url: Some("https://mcp.example.test".to_string()),
                sign_in_host: Some("auth.example.test".to_string()),
            },
            McpOAuthCredentials {
                access_token: "access".to_string(),
                refresh_token: refresh.then(|| "refresh".to_string()),
                expires_at_unix: if fresh {
                    now + 3600
                } else {
                    now.saturating_sub(10)
                },
                scope: None,
            },
        )
    }

    fn sign_in() -> OAuthNeed {
        OAuthNeed::SignIn {
            host: "auth.example.test".to_string(),
        }
    }

    fn state_of(status: Option<McpOAuthStatus>) -> Option<McpOAuthState> {
        status.map(|status| status.state)
    }

    /// A plain HTTP server that never asked for OAuth shows no OAuth control,
    /// and one that asked shows Connect whatever its saved flag says, with
    /// the host Connect sends the person to.
    #[test]
    fn only_a_server_that_asks_for_oauth_shows_a_status() {
        assert_eq!(project_status(false, None, None, None), None);
        let status = project_status(false, Some(&sign_in()), None, None).unwrap();
        assert_eq!(status.state, McpOAuthState::NotConnected);
        assert_eq!(status.sign_in_host.as_deref(), Some("auth.example.test"));
        assert_eq!(
            state_of(project_status(true, None, None, None)),
            Some(McpOAuthState::NotConnected)
        );
    }

    #[test]
    fn a_pending_sign_in_wins_and_carries_its_page() {
        let pending = SignInView::Pending {
            url: "https://auth.example.test/authorize".to_string(),
            host: "auth.example.test".to_string(),
        };
        let status = project_status(
            false,
            Some(&sign_in()),
            Some(&pending),
            Some(&session(true, true)),
        )
        .unwrap();
        assert_eq!(status.state, McpOAuthState::Authorizing);
        assert_eq!(
            status.pending_authorization_url.as_deref(),
            Some("https://auth.example.test/authorize")
        );
        assert_eq!(status.sign_in_host.as_deref(), Some("auth.example.test"));
    }

    /// A stored session the server still refuses must not read as connected.
    #[test]
    fn a_refused_session_reads_as_expired_not_connected() {
        let status =
            project_status(false, Some(&sign_in()), None, Some(&session(true, true))).unwrap();
        assert_eq!(status.state, McpOAuthState::Expired);
        assert_eq!(status.error.as_deref(), Some(SESSION_REJECTED));

        let connected = project_status(false, None, None, Some(&session(true, false))).unwrap();
        assert_eq!(connected.state, McpOAuthState::Connected);
        assert_eq!(connected.sign_in_host.as_deref(), Some("auth.example.test"));
        assert_eq!(
            state_of(project_status(
                false,
                None,
                None,
                Some(&session(false, true))
            )),
            Some(McpOAuthState::Connected)
        );
        assert_eq!(
            state_of(project_status(
                false,
                None,
                None,
                Some(&session(false, false))
            )),
            Some(McpOAuthState::Expired)
        );
    }

    /// A failed re-sign-in does not hide a session that still works, and a
    /// failure with nothing stored says what went wrong.
    #[test]
    fn a_failure_explains_itself_unless_a_session_still_works() {
        let failed = SignInView::Failed(
            McpOAuthState::AccessDenied,
            "The sign-in was canceled or denied.".to_string(),
        );
        assert_eq!(
            state_of(project_status(
                false,
                None,
                Some(&failed),
                Some(&session(true, true))
            )),
            Some(McpOAuthState::Connected)
        );
        let status = project_status(false, Some(&sign_in()), Some(&failed), None).unwrap();
        assert_eq!(status.state, McpOAuthState::AccessDenied);
        assert_eq!(
            status.error.as_deref(),
            Some("The sign-in was canceled or denied.")
        );
    }

    #[test]
    fn an_unsupported_server_says_why() {
        let status = project_status(
            false,
            Some(&OAuthNeed::Unsupported(
                OAuthUnsupported::NoClientRegistration,
            )),
            None,
            None,
        )
        .unwrap();
        assert_eq!(status.state, McpOAuthState::Unsupported);
        assert_eq!(
            status.error.as_deref(),
            Some(OAuthUnsupported::NoClientRegistration.reason())
        );
    }

    /// A sign-in service that did not answer is temporary: Connect stays on
    /// offer, the row says so, and the supervisor keeps retrying.
    #[test]
    fn an_unanswered_sign_in_service_is_temporary() {
        let status = project_status(false, Some(&OAuthNeed::Unavailable), None, None).unwrap();
        assert_eq!(status.state, McpOAuthState::NotConnected);
        assert_eq!(status.error.as_deref(), Some(SERVICE_DOWN));
        assert!(!OAuthNeed::Unavailable.parks());
        assert!(OAuthNeed::Unavailable.saves());
        assert!(sign_in().parks());
        assert!(sign_in().saves());
        let unsupported = OAuthNeed::Unsupported(OAuthUnsupported::NoS256);
        assert!(unsupported.parks());
        assert!(!unsupported.saves());
    }

    #[test]
    fn only_servers_that_can_carry_a_token_sign_in() {
        let http = |url: &str| McpServerDefinition {
            name: "docs".to_string(),
            command: None,
            args: Vec::new(),
            env: Default::default(),
            env_values: Default::default(),
            env_from: Vec::new(),
            cwd: None,
            approved_executable: None,
            url: Some(url.to_string()),
            bearer_token_env: None,
            bearer_token_stored: false,
            bearer_token_value: None,
            headers: Default::default(),
            header_values: Default::default(),
            oauth: false,
            gateway_endpoint: None,
            request_timeout_ms: super::super::types::DEFAULT_REQUEST_TIMEOUT_MS,
            enabled: true,
            plugin: None,
            launch: None,
        };
        assert!(signs_in(&http("https://mcp.example.test/mcp")));
        assert!(signs_in(&http("http://127.0.0.1:8080/mcp")));
        assert_eq!(
            sign_in_url(&http("https://mcp.example.test/mcp")),
            Some("https://mcp.example.test/mcp")
        );
        // Cleartext to another host can never carry the token.
        assert!(!signs_in(&http("http://mcp.example.test/mcp")));

        let mut bearer = http("https://mcp.example.test/mcp");
        bearer.bearer_token_env = Some("MCP_TOKEN".to_string());
        assert!(!signs_in(&bearer));
        assert_eq!(sign_in_url(&bearer), None);
        assert!(may_offer_sign_in(&bearer));

        // A bearer held in the credential store is just as static.
        let mut stored = http("https://mcp.example.test/mcp");
        stored.bearer_token_stored = true;
        assert!(!signs_in(&stored));
        assert!(may_offer_sign_in(&stored));
        assert!(!may_offer_sign_in(&http("https://mcp.example.test/mcp")));

        let mut plugin = http("https://mcp.example.test/mcp");
        plugin.plugin = Some("docs-plugin".to_string());
        assert!(!signs_in(&plugin));
    }
}
