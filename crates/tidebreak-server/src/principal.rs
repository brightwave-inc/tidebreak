//! Who is asking — resolved once at the authentication boundary.
//!
//! The per-launch credentials in [`crate::auth`] are capability checks: they
//! prove the caller was handed a secret by this process, not who the caller
//! is. This module gives request handling a typed answer to "who is asking"
//! so authorization can grow from "the token is valid" to "this principal may
//! see this row" without re-plumbing the request path (#853).
//!
//! Two rules are load-bearing:
//!
//! - **The context is attached only by the auth middleware.** [`AuthContext`]
//!   is an extractor that fails closed: a handler that asks for it on a route
//!   the auth layer does not cover gets `401`, never a defaulted identity.
//! - **The client-executor credential is a capability, not a principal.** It
//!   marks a bit on an existing context; it cannot conjure an identity on its
//!   own. [`ClientExecutor`] types that requirement into handler signatures so
//!   it survives router refactors.
//!
//! A principal also carries a [`Role`]: the split between the member plane
//! (owner-scoped data and capability discovery) and the deployment plane
//! (what the deployment *is*, and its shared secrets) is enforced by
//! [`crate::auth::require_admin`] over the role resolved here — see
//! `docs/decisions/0006-self-host-deployment-plane-authorization.md`.

use std::fmt;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use tidebreak_core::{AgentError, OwnerId, Result};

/// What a principal is permitted to reach.
///
/// Two values, deliberately: the concrete gap the split closes is binary —
/// host command execution and shared-secret custody versus everything else.
/// A grant vocabulary would be guessed until someone asks for delegated
/// partial administration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// May reconfigure the deployment and hold its shared secrets.
    Admin,
    /// May use the deployment and see their own data. Everything else is a
    /// `403`.
    Member,
}

/// The authenticated identity a request acts as.
///
/// The desktop bearer is per-launch and loopback-only, so whoever presents it
/// is the one person at the machine — [`Principal::LocalOwner`]. On the shared
/// self-host profile every credential names a configured user instead
/// ([`Principal::User`]); a token that names no one is rejected at the
/// middleware, and the local-owner identity does not exist there. Nothing
/// downstream may assume `LocalOwner` is the only case.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Principal {
    /// The one person at the machine, authenticated by the per-launch bearer.
    LocalOwner,
    /// A named user on a shared deployment, resolved from a configured
    /// credential (today the self-host token file — see [`crate::auth`]).
    User {
        /// Who the credential names.
        id: UserId,
        /// What the token file says they may reconfigure.
        role: Role,
    },
}

impl Principal {
    /// The durable storage key this principal's rows are attributed to and
    /// scoped by (#853).
    ///
    /// The local owner maps to the fixed [`OwnerId::local`] key; a named user
    /// maps to `user:<id>`. The prefix keeps the two namespaces disjoint —
    /// [`UserId`] rejects `:`, so no operator-assigned user id can collide
    /// with `local` or with another user's key.
    #[must_use]
    pub fn owner_id(&self) -> OwnerId {
        match self {
            Self::LocalOwner => OwnerId::local(),
            Self::User { id, .. } => OwnerId::new(&format!("user:{id}"))
                .expect("a validated user id forms a valid owner key"),
        }
    }

    /// Whether this principal may reach the deployment plane.
    ///
    /// The local owner is unconditionally admin: the desktop profile is one
    /// person at one machine, and there is nobody to be an administrator over.
    #[must_use]
    pub fn is_admin(&self) -> bool {
        match self {
            Self::LocalOwner => true,
            Self::User { role, .. } => *role == Role::Admin,
        }
    }
}

/// The operator-assigned identifier of a named user on a shared deployment.
///
/// Validated at construction: 1–64 ASCII characters from `[A-Za-z0-9._@-]`.
/// The id is an identity label, not a secret — it appears in config files and
/// (in later slices) owner columns, so it is kept greppable and log-safe.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UserId(Arc<str>);

impl UserId {
    /// Validate and intern a user id.
    pub fn new(id: &str) -> Result<Self> {
        let valid = !id.is_empty()
            && id.len() <= 64
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'@' | b'-'));
        if valid {
            Ok(Self(id.into()))
        } else {
            Err(AgentError::config(format!(
                "invalid user id {id:?}: expected 1-64 characters from [A-Za-z0-9._@-]"
            )))
        }
    }
}

impl fmt::Display for UserId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The authentication result the middleware attaches to a verified request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    /// Who the request acts as.
    pub principal: Principal,
    /// Whether the request also presented the native client-executor
    /// credential. Host authority (claiming delegated work, touching the
    /// filesystem), not identity — it never widens what `principal` may see.
    pub client_executor: bool,
}

impl<S: Send + Sync> FromRequestParts<S> for AuthContext {
    /// Absence means the auth middleware never ran for this route — an
    /// unauthenticated request, as far as any identity consumer is concerned.
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AuthContext>()
            .cloned()
            .ok_or(StatusCode::UNAUTHORIZED)
    }
}

/// Proof the request presented the native client-executor credential on top of
/// an authenticated principal.
///
/// Handlers on the native-only surface take this so the requirement is part of
/// the handler's type, not only of which router it happens to be mounted on.
/// When owner-scoped queries land, this grows the principal the capability was
/// granted on; until a consumer exists it stays a bare proof token.
#[derive(Debug, Clone, Copy)]
pub struct ClientExecutor;

impl<S: Send + Sync> FromRequestParts<S> for ClientExecutor {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let auth = AuthContext::from_request_parts(parts, state).await?;
        if auth.client_executor {
            Ok(Self)
        } else {
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    /// The boundary contract: a route the auth middleware does not cover
    /// cannot observe an identity — the extractors fail closed instead of
    /// defaulting to the local owner.
    #[tokio::test]
    async fn extractors_fail_closed_without_the_auth_middleware() {
        let app = Router::new()
            .route("/principal", get(|_auth: AuthContext| async { "leaked" }))
            .route(
                "/capability",
                get(|_executor: ClientExecutor| async { "leaked" }),
            );

        for uri in ["/principal", "/capability"] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{uri} must reject when no middleware attached a context"
            );
        }
    }

    /// A principal without the native credential is not a client executor.
    #[tokio::test]
    async fn capability_is_not_implied_by_identity() {
        let app = Router::new()
            .route("/", get(|_executor: ClientExecutor| async { "leaked" }))
            .layer(axum::middleware::from_fn(
                |mut request: Request<Body>, next: axum::middleware::Next| async move {
                    request.extensions_mut().insert(AuthContext {
                        principal: Principal::LocalOwner,
                        client_executor: false,
                    });
                    next.run(request).await
                },
            ));

        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
