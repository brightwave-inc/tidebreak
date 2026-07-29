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
//! egress and before they can reach a model context. Both adapters implement
//! page extraction as well as search, on the same fixed authority; a per-URL
//! vendor failure is a typed error rather than an empty success, so extraction
//! can fall back to the native engine instead of returning a blank page.
//!
//! The `extract-native` feature adds a self-contained extraction engine:
//! [`NativeExtractor`] fetches one admitted URL — every redirect hop re-vetted
//! against the [`fetch_policy`](admit_fetch_url) admission rules with fresh,
//! pinned DNS resolution — and reduces the page to bounded readable markdown.
//! The admission policy itself is always compiled because it is pure and
//! dependency-light; only the parser and transport stack is feature-gated.

mod credentials;
pub mod exa;
mod fetch_policy;
mod http;
#[cfg(feature = "extract-native")]
mod native;
pub mod tavily;
mod tool;
mod types;

pub use credentials::{WebSearchCredential, WebSearchCredentials};
pub use exa::ExaProvider;
pub use fetch_policy::{
    admit_fetch_address, admit_fetch_url, FetchPolicyViolation, MAX_FETCH_URL_BYTES,
};
#[cfg(feature = "http")]
pub use http::ReqwestHttpClient;
pub use http::{HttpClient, HttpRequest, HttpResponse, MAX_HTTP_RESPONSE_BYTES};
#[cfg(feature = "extract-native")]
pub use native::{
    HostAddressResolver, NativeExtractError, NativeExtraction, NativeExtractor, PageFetchResponse,
    PageFetchTransport, ReqwestPageFetcher, TokioHostResolver, MAX_EXTRACT_CONTENT_CHARS,
    MAX_FETCH_REDIRECT_HOPS, MAX_FETCH_RESPONSE_BYTES, NATIVE_FETCH_USER_AGENT,
};
pub use tavily::TavilyProvider;
pub use tool::{
    extract_request_from_tool_arguments, request_from_tool_arguments, PageExtractor,
    WebExtractTool, WebSearchResolver, WebSearchResolverError, WebSearchTool, DEFAULT_MAX_RESULTS,
};
pub use types::{
    ExtractionMethod, SearchDomain, WebExtractFailure, WebExtractRequest, WebExtractResponse,
    WebSearchError, WebSearchProvider, WebSearchProviderKind, WebSearchRequest, WebSearchResponse,
    WebSearchResult, MAX_DOMAINS, MAX_EXTRACT_OUTPUT_BYTES, MAX_OUTPUT_BYTES, MAX_OUTPUT_CHARS,
    MAX_QUERY_CHARS, MAX_RESULTS, MAX_RESULT_CONTENT_CHARS, MAX_RESULT_SNIPPET_CHARS,
    MAX_RESULT_TITLE_CHARS, MAX_RESULT_URL_BYTES, MIN_EXTRACT_WORDS,
};
