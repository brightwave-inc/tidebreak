//! Request extractors that map axum's built-in rejections into [`ServerError`].
//!
//! axum's stock `Json`/`Path` reject a malformed request with a plain-text body
//! and a bare status. These thin wrappers delegate to them but convert any
//! rejection into a `ServerError`, so *every* failure — bad path segment,
//! unparseable body, wrong/absent `Content-Type` — answers with the same
//! `{ kind, message }` JSON a client can always parse.

use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::ServerError;

/// Like [`axum::Json`], but a parse or content-type failure becomes a `400` with
/// a JSON `{ kind, message }` body. Also serializes as a JSON response body, so
/// handlers use this one type for both request and response.
pub struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ServerError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let axum::Json(value) = axum::Json::<T>::from_request(req, state)
            .await
            .map_err(|rejection: JsonRejection| ServerError::bad_request(rejection.body_text()))?;
        Ok(Self(value))
    }
}

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

/// Like [`axum::extract::Path`], but an unparseable path segment (e.g. a
/// non-UUID id) becomes a `400` with a JSON `{ kind, message }` body.
pub struct Path<T>(pub T);

impl<T, S> FromRequestParts<S> for Path<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ServerError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let axum::extract::Path(value) = axum::extract::Path::<T>::from_request_parts(parts, state)
            .await
            .map_err(|rejection: PathRejection| ServerError::bad_request(rejection.body_text()))?;
        Ok(Self(value))
    }
}
