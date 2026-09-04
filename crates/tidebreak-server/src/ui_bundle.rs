//! Serving the renderer bundle to browsers.
//!
//! A hosted machine has nowhere to land a person until it serves a page.
//! Configured with a built desktop UI bundle (`TIDEBREAK_UI_DIST`), the
//! server answers page navigations for any path the API does not own with
//! that bundle, and serves its files by name. The bundle is the same `dist`
//! the packaged desktop app loads over its own protocol, so a browser tab and
//! the desktop run one renderer against one API.
//!
//! The fallback is deliberately narrow. Only `GET` and `HEAD` reach the
//! bundle, and `index.html` stands in for an unknown path only when the
//! request is a navigation — an `Accept` that names `text/html`. A `fetch`
//! for a route that does not exist keeps its `404`, so a renderer newer than
//! its server still learns that an endpoint is missing instead of parsing a
//! page as JSON.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use tidebreak_core::AgentError;
use tower_http::services::ServeDir;

/// The renderer's compiled assets carry a content hash in their name, so a
/// changed bundle is a changed URL and the old file can be held forever.
const IMMUTABLE_ASSET_PREFIX: &str = "/assets/";
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
/// Everything else — `index.html` above all — is fetched by a stable name and
/// must be revalidated, or a tab keeps an old bundle after an image update.
const REVALIDATE: &str = "no-cache";

/// Refuse a configured directory that cannot serve a page. Checked at bind,
/// so an image that forgot to copy its bundle fails before it listens rather
/// than answering every navigation with a `404`.
pub fn verify(dist: &Path) -> Result<(), AgentError> {
    let index = dist.join("index.html");
    if index.is_file() {
        return Ok(());
    }
    Err(AgentError::config(format!(
        "TIDEBREAK_UI_DIST names {}, which holds no index.html; point it at a built \
         renderer bundle or unset it to serve no pages",
        dist.display()
    )))
}

/// The router fallback: a file from the bundle, `index.html` for a page
/// navigation, and a `404` for everything else.
pub async fn serve(dist: Arc<PathBuf>, request: Request) -> Response {
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let navigation = accepts_html(request.headers());
    let path = request.uri().path().to_owned();
    let served = ServeDir::new(dist.as_path()).try_call(request).await;
    match served {
        Ok(response) if response.status() != StatusCode::NOT_FOUND => {
            with_cache_policy(response.map(Body::new), &path)
        }
        Ok(_) if navigation => index(&dist).await,
        Ok(_) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn index(dist: &Path) -> Response {
    let request = Request::builder()
        .uri("/index.html")
        .body(Body::empty())
        .expect("a literal request line is well-formed");
    match ServeDir::new(dist).try_call(request).await {
        Ok(response) if response.status() == StatusCode::OK => {
            with_cache_policy(response.map(Body::new), "/index.html")
        }
        // Verified at bind, so this is the bundle disappearing from under a
        // running server. Say so rather than pretend the path is unknown.
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn accepts_html(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"))
}

fn with_cache_policy(mut response: Response, path: &str) -> Response {
    let policy = if path.starts_with(IMMUTABLE_ASSET_PREFIX) {
        IMMUTABLE
    } else {
        REVALIDATE
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(policy));
    response
}
