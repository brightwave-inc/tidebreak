//! Provider-neutral, bounded web search for OpenWave.
//!
//! This crate deliberately does not register a model tool, load credentials, or
//! make network calls at construction time. A host explicitly resolves a
//! provider's key from [`openwave_core::SecretProvider`], constructs an adapter,
//! and calls [`WebSearchProvider::search`]. The concrete HTTP client binds each
//! adapter to its provider's exact HTTPS domain before any tool or worker can
//! use it.
//!
//! The Exa and Tavily adapters use their public HTTP APIs through the small
//! [`HttpClient`] seam; no vendor SDK is required. `ReqwestHttpClient` is opt-in
//! behind the `http` feature. Requests and normalized output are bounded before
//! egress and before they can reach a model context.

mod credentials;
pub mod exa;
mod http;
pub mod tavily;
mod tool;
mod types;

pub use credentials::{WebSearchCredential, WebSearchCredentials};
pub use exa::ExaProvider;
#[cfg(feature = "http")]
pub use http::ReqwestHttpClient;
pub use http::{HttpClient, HttpRequest, HttpResponse, MAX_HTTP_RESPONSE_BYTES};
pub use tavily::TavilyProvider;
pub use tool::{
    request_from_tool_arguments, WebSearchResolver, WebSearchResolverError, WebSearchTool,
    DEFAULT_MAX_RESULTS,
};
pub use types::{
    SearchDomain, WebSearchError, WebSearchProvider, WebSearchProviderKind, WebSearchRequest,
    WebSearchResponse, WebSearchResult, MAX_DOMAINS, MAX_OUTPUT_BYTES, MAX_OUTPUT_CHARS,
    MAX_QUERY_CHARS, MAX_RESULTS, MAX_RESULT_CONTENT_CHARS, MAX_RESULT_SNIPPET_CHARS,
    MAX_RESULT_TITLE_CHARS, MAX_RESULT_URL_BYTES,
};
