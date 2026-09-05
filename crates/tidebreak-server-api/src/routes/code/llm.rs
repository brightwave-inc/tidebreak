//! `/code/llm/*` routes: the engine-facing inference relay.
//!
//! These routes authenticate with the per-session relay key minted by
//! [`crate::code::harness_llm::HarnessLlmRelay`] — never the per-launch app
//! token. They are registered outside `require_token` (see `crate::app`),
//! and the relay resolves the key from each request's own headers.

use axum::body::Body;
use axum::extract::{RawQuery, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::code::harness_llm::RelayEndpoint;
use crate::obo_gateway::GitForgeError;
use crate::state::AppState;

/// Body cap for one relayed inference request. A long session's request
/// carries its whole context, so this is far above the default limit and
/// still bounded: the gateway enforces its own ceiling behind it.
pub(crate) const MAX_HARNESS_LLM_BODY_BYTES: usize = 64 * 1024 * 1024;

pub async fn harness_llm_anthropic_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Body,
) -> Response {
    relay(
        state,
        RelayEndpoint::AnthropicMessages,
        headers,
        query,
        body,
    )
    .await
}

pub async fn harness_llm_openai_responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Body,
) -> Response {
    relay(state, RelayEndpoint::OpenAiResponses, headers, query, body).await
}

pub async fn harness_llm_openai_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    match state.code.as_ref().and_then(|code| code.harness_llm()) {
        Some(relay) => {
            relay
                .forward_models(RelayEndpoint::OpenAiModels, &headers)
                .await
        }
        // The desktop and static-token self-host profiles run engines on the
        // machine's own provider configuration; they mint no relay keys, so
        // nothing legitimate calls this.
        None => RelayEndpoint::OpenAiModels.error_response(
            StatusCode::NOT_FOUND,
            "not_found_error",
            "this machine has no harness inference relay",
        ),
    }
}

async fn relay(
    state: AppState,
    endpoint: RelayEndpoint,
    headers: HeaderMap,
    query: Option<String>,
    body: Body,
) -> Response {
    match state.code.as_ref().and_then(|code| code.harness_llm()) {
        Some(relay) => relay.forward(endpoint, &headers, query, body).await,
        // The desktop and static-token self-host profiles run engines on the
        // machine's own provider configuration; they mint no relay keys, so
        // nothing legitimate calls this.
        None => endpoint.error_response(
            StatusCode::NOT_FOUND,
            "not_found_error",
            "this machine has no harness inference relay",
        ),
    }
}

/// Body cap for one git credential description: a handful of `key=value`
/// lines.
pub(crate) const MAX_GIT_CREDENTIAL_BODY_BYTES: usize = 16 * 1024;

/// `POST /code/git/credential` — git's credential protocol for a machine
/// session's own `git`, authenticated by the session's relay key.
///
/// The body is the description git hands a helper (`protocol=`, `host=`,
/// `path=`, one per line). A credential is lent only for `https` against
/// the exact host the session's repository was cloned from, minted through
/// the same person path the server's own git uses (decision 63), and
/// answered as `username=` and `password=` lines. Any other description
/// gets an empty answer, which git reads as "no credential", never as an
/// error; so does a session with no repository.
pub async fn harness_git_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let Some(code) = state.code.as_ref() else {
        return plain(StatusCode::NOT_FOUND, "this machine runs no code sessions");
    };
    let Some(relay) = code.harness_llm() else {
        return plain(
            StatusCode::NOT_FOUND,
            "this machine lends no git credentials",
        );
    };
    let Some(subject) = relay.subject_for_headers(&headers) else {
        return plain(
            StatusCode::UNAUTHORIZED,
            "unknown or revoked harness relay key",
        );
    };
    let Some(lender) = code.git_credentials() else {
        return plain(
            StatusCode::NOT_FOUND,
            "this machine lends no git credentials",
        );
    };
    let session = match code.get_session(&subject.owner, subject.session).await {
        Ok(session) => session,
        Err(_) => return plain(StatusCode::NOT_FOUND, "session not found"),
    };
    let Some(workspace_id) = session.workspace_id else {
        return plain(StatusCode::OK, "");
    };
    let repo = match code.get_workspace(&subject.owner, workspace_id).await {
        Ok(workspace) => match code.get_repo(&subject.owner, workspace.repo_id).await {
            Ok(repo) => repo,
            Err(_) => return plain(StatusCode::NOT_FOUND, "repository not found"),
        },
        Err(_) => return plain(StatusCode::NOT_FOUND, "workspace not found"),
    };
    let (Some(origin_host), Some(origin_owner), Some(origin_name)) =
        (repo.origin_host, repo.origin_owner, repo.origin_name)
    else {
        return plain(StatusCode::OK, "");
    };
    let asked = GitCredentialAsk::parse(&body);
    if asked.protocol.as_deref() != Some("https")
        || !asked
            .host
            .as_deref()
            .is_some_and(|host| host.eq_ignore_ascii_case(&origin_host))
    {
        return plain(StatusCode::OK, "");
    }
    let delegated = match relay
        .external_gateway_for_session(&subject.owner, subject.session)
        .await
    {
        Ok(gateway) => gateway,
        Err(
            error @ (tidebreak_core::AgentError::SignInRequired(_)
            | tidebreak_core::AgentError::InvalidTarget(_)),
        ) => return plain(StatusCode::UNAUTHORIZED, &error.to_string()),
        Err(error) => return plain(StatusCode::BAD_GATEWAY, &error.to_string()),
    };
    let lender: &dyn crate::obo_gateway::GitCredentialLender = match delegated.as_ref() {
        Some(gateway) => gateway.as_ref(),
        None => lender.as_ref(),
    };
    match lender
        .git_credential(&subject.owner, &format!("{origin_owner}/{origin_name}"))
        .await
    {
        Ok(credential) => plain(
            StatusCode::OK,
            &format!(
                "username={}\npassword={}\n",
                credential.username, credential.secret
            ),
        ),
        Err(GitForgeError::SignInRequired(message)) => plain(StatusCode::UNAUTHORIZED, &message),
        Err(error) => plain(
            StatusCode::BAD_GATEWAY,
            &crate::code::clone::git_forge_refusal_message(&error),
        ),
    }
}

/// What git asked for, from the description lines a helper receives.
#[derive(Default)]
struct GitCredentialAsk {
    protocol: Option<String>,
    host: Option<String>,
}

impl GitCredentialAsk {
    fn parse(body: &str) -> Self {
        let mut asked = Self::default();
        for line in body.lines() {
            match line.split_once('=') {
                Some(("protocol", value)) => asked.protocol = Some(value.trim().to_owned()),
                // A `host` may carry a port; git compares the whole value,
                // and so does the origin pin.
                Some(("host", value)) => asked.host = Some(value.trim().to_owned()),
                _ => {}
            }
        }
        asked
    }
}

fn plain(status: StatusCode, body: &str) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body.to_owned(),
    )
        .into_response()
}
