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

use std::sync::Arc;
use std::time::Duration;

use tidebreak_core::id::ConnectedAppId;
use tidebreak_core::SecretProvider;

use crate::connectors::{Discovery, McpOAuthClient, McpOAuthCredentials, OAuthUnsupported};
use crate::mcp_oauth_runtime::{McpOAuthState, McpOAuthStatus};

use super::types::McpServerDefinition;

/// How long the runtime waits for a server's `401` challenge when it asks the
/// server how to authorize.
pub(super) const CHALLENGE_TIMEOUT: Duration = Duration::from_secs(10);

/// The most asking a server how to authorize may add to a failed connection.
/// A server that takes longer keeps its `401`, and the next attempt asks again.
const DETECTION_TIMEOUT: Duration = Duration::from_secs(30);

/// The shared reason for an HTTP server that has no OAuth sign-in to run.
pub(super) const NO_OAUTH: &str = "This server does not ask for an OAuth sign-in.";

/// Why Connect could not start for a server known to sign in: its sign-in
/// metadata did not load this time.
pub(super) const METADATA_UNREADABLE: &str = "Tidebreak could not read this server's sign-in \
                                              settings. Check your connection, then select \
                                              Connect to try again.";

/// What a refused connection taught the runtime about a server's sign-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OAuthNeed {
    /// The server asks for an OAuth sign-in Tidebreak can run. The person
    /// signs in with Connect.
    SignIn,
    /// The server asks for an OAuth sign-in Tidebreak cannot complete.
    Unsupported(OAuthUnsupported),
}

impl OAuthNeed {
    /// What Settings says about a server in this state. Never names a URL.
    pub(super) fn diagnostic(self) -> String {
        match self {
            Self::SignIn => {
                "This server needs you to sign in. Select Connect to sign in with your browser."
                    .to_string()
            }
            Self::Unsupported(reason) => format!(
                "This server asks you to sign in, but Tidebreak cannot complete its sign-in. {} \
                 If the server offers access tokens, set a bearer token variable instead.",
                reason.reason()
            ),
        }
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
        && definition
            .url
            .as_deref()
            .is_some_and(|url| tidebreak_mcp::validate_http_url_with_credentials(url, true).is_ok())
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
            Discovery::Supported(_) => Some(OAuthNeed::SignIn),
            Discovery::Unsupported(reason) => Some(OAuthNeed::Unsupported(reason)),
        }
    };
    tokio::time::timeout(DETECTION_TIMEOUT, ask)
        .await
        .ok()
        .flatten()
}

/// One server's sign-in while it runs, and after it fails.
pub(super) enum SignInProgress {
    /// The person's browser has the authorization page; the loopback
    /// listener waits for its redirect.
    Pending {
        authorization_url: String,
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
    Pending(String),
    Failed(McpOAuthState, String),
}

impl SignInProgress {
    pub(super) fn view(&self) -> SignInView {
        match self {
            Self::Pending {
                authorization_url, ..
            } => SignInView::Pending(authorization_url.clone()),
            Self::Failed { state, message } => SignInView::Failed(*state, message.clone()),
        }
    }
}

/// The sentence for a stored session the server no longer accepts.
const SESSION_REJECTED: &str =
    "Your sign-in is no longer valid. Select Reconnect to sign in again.";

/// The sentence for a stored session that expired with no way to refresh it.
const SESSION_EXPIRED: &str = "Your sign-in expired. Select Reconnect to sign in again.";

/// The OAuth status Settings shows for one server that [`signs_in`], or
/// `None` when nothing about the server involves OAuth.
///
/// `flag` is the saved `oauth` setting, `need` what the last connection
/// attempt learned, `progress` the last sign-in, and `stored` the session in
/// the credential store. A sign-in in flight wins. A usable session the
/// server has not refused reads as connected, even after a later sign-in
/// failed. Otherwise the last failure explains itself, then what the server
/// asked for, then what is stored.
pub(super) fn project_status(
    flag: bool,
    need: Option<OAuthNeed>,
    progress: Option<&SignInView>,
    stored: Option<&McpOAuthCredentials>,
) -> Option<McpOAuthStatus> {
    if let Some(SignInView::Pending(url)) = progress {
        return Some(McpOAuthStatus::authorizing(url.clone()));
    }
    let usable = stored.is_some_and(|credentials| {
        credentials.access_is_fresh() || credentials.refresh_token.is_some()
    });
    if usable && need.is_none() {
        return Some(McpOAuthStatus::connected());
    }
    if let Some(SignInView::Failed(state, message)) = progress {
        return Some(McpOAuthStatus::failed(*state, message.clone()));
    }
    match need {
        Some(OAuthNeed::Unsupported(reason)) => Some(McpOAuthStatus::failed(
            McpOAuthState::Unsupported,
            reason.reason(),
        )),
        Some(OAuthNeed::SignIn) if stored.is_some() => Some(McpOAuthStatus::failed(
            McpOAuthState::Expired,
            SESSION_REJECTED,
        )),
        Some(OAuthNeed::SignIn) => Some(McpOAuthStatus::not_connected()),
        None if stored.is_some() => Some(McpOAuthStatus::failed(
            McpOAuthState::Expired,
            SESSION_EXPIRED,
        )),
        None if flag => Some(McpOAuthStatus::not_connected()),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(fresh: bool, refresh: bool) -> McpOAuthCredentials {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        McpOAuthCredentials {
            access_token: "access".to_string(),
            refresh_token: refresh.then(|| "refresh".to_string()),
            expires_at_unix: if fresh {
                now + 3600
            } else {
                now.saturating_sub(10)
            },
            scope: None,
        }
    }

    fn state_of(status: Option<McpOAuthStatus>) -> Option<McpOAuthState> {
        status.map(|status| status.state)
    }

    /// A plain HTTP server that never asked for OAuth shows no OAuth control,
    /// and one that asked shows Connect whatever its saved flag says.
    #[test]
    fn only_a_server_that_asks_for_oauth_shows_a_status() {
        assert_eq!(project_status(false, None, None, None), None);
        assert_eq!(
            state_of(project_status(false, Some(OAuthNeed::SignIn), None, None)),
            Some(McpOAuthState::NotConnected)
        );
        assert_eq!(
            state_of(project_status(true, None, None, None)),
            Some(McpOAuthState::NotConnected)
        );
    }

    #[test]
    fn a_pending_sign_in_wins_and_carries_its_page() {
        let pending = SignInView::Pending("https://auth.example.test/authorize".to_string());
        let status = project_status(
            false,
            Some(OAuthNeed::SignIn),
            Some(&pending),
            Some(&session(true, true)),
        )
        .unwrap();
        assert_eq!(status.state, McpOAuthState::Authorizing);
        assert_eq!(
            status.pending_authorization_url.as_deref(),
            Some("https://auth.example.test/authorize")
        );
    }

    /// A stored session the server still refuses must not read as connected.
    #[test]
    fn a_refused_session_reads_as_expired_not_connected() {
        let status = project_status(
            false,
            Some(OAuthNeed::SignIn),
            None,
            Some(&session(true, true)),
        )
        .unwrap();
        assert_eq!(status.state, McpOAuthState::Expired);
        assert_eq!(status.error.as_deref(), Some(SESSION_REJECTED));

        assert_eq!(
            state_of(project_status(
                false,
                None,
                None,
                Some(&session(true, false))
            )),
            Some(McpOAuthState::Connected)
        );
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
        let status = project_status(false, Some(OAuthNeed::SignIn), Some(&failed), None).unwrap();
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
            Some(OAuthNeed::Unsupported(
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
            url: Some(url.to_string()),
            bearer_token_env: None,
            oauth: false,
            gateway_endpoint: None,
            request_timeout_ms: super::super::types::DEFAULT_REQUEST_TIMEOUT_MS,
            enabled: true,
            plugin: None,
            launch: None,
        };
        assert!(signs_in(&http("https://mcp.example.test/mcp")));
        assert!(signs_in(&http("http://127.0.0.1:8080/mcp")));
        // Cleartext to another host can never carry the token.
        assert!(!signs_in(&http("http://mcp.example.test/mcp")));

        let mut bearer = http("https://mcp.example.test/mcp");
        bearer.bearer_token_env = Some("MCP_TOKEN".to_string());
        assert!(!signs_in(&bearer));

        let mut plugin = http("https://mcp.example.test/mcp");
        plugin.plugin = Some("docs-plugin".to_string());
        assert!(!signs_in(&plugin));
    }
}
