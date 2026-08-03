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

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;

/// The authenticated identity a request acts as.
///
/// Today the only inhabitant is the single local owner: the desktop bearer is
/// per-launch and loopback-only, so whoever presents it is the one person at
/// the machine. A shared deployment adds a variant whose token names a user;
/// nothing downstream may assume `LocalOwner` is the only case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Principal {
    /// The one person at the machine, authenticated by the per-launch bearer.
    LocalOwner,
}

/// The authentication result the middleware attaches to a verified request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
            .copied()
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
