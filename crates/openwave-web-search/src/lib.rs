//! Provider-neutral, bounded web search for OpenWave.
//!
//! This crate deliberately does not register a model tool, load credentials, or
//! make network calls at construction time. A host explicitly resolves a
//! provider's key from [`openwave_core::SecretProvider`], constructs an adapter,
//! and calls [`WebSearchProvider::search`]. The concrete HTTP client binds each
//! adapter to its provider's exact HTTPS domain before any tool or worker can
//! use it.
//!
//! Every adapter talks to its backend over plain HTTP through the small
//! [`HttpClient`] seam; no vendor SDK is used, and none should be added. Cargo
//! features are resolved at compile time, so an SDK for a backend most users
//! never configure would still ship in every binary, while an HTTP adapter
//! reuses the client that is already there and costs essentially nothing per
//! backend. `ReqwestHttpClient` is opt-in behind the `http` feature. Requests
//! and normalized output are bounded before egress and before they can reach a
//! model context.
//!
//! Exa and Tavily implement page extraction as well as search, on the same
//! fixed authority; a per-URL vendor failure is a typed error rather than an
//! empty success, so extraction can fall back to the native engine instead of
//! returning a blank page. Brave and SearXNG are search-only and say so through
//! [`WebSearchProvider::supports_extract`], which sends extraction to the
//! native engine.
//!
//! SearXNG is self-hosted, which makes it the one provider that carries no
//! credential and the one whose address is host configuration rather than a
//! fixed constant. Both departures are contained in [`searxng`]; the
//! credentialed providers still fail closed on a missing key, and the transport
//! is still bound to exactly one [`OutboundOrigin`] before any request exists.
//!
//! The `extract-native` feature adds a self-contained extraction engine:
//! [`NativeExtractor`] fetches one admitted URL — every redirect hop re-vetted
//! against the [`fetch_policy`](admit_fetch_url) admission rules with fresh,
//! pinned DNS resolution — and reduces the page to bounded readable markdown.
//! The admission policy itself is always compiled because it is pure and
//! dependency-light; only the parser and transport stack is feature-gated.

pub mod brave;
mod credentials;
pub mod exa;
mod extract_source;
mod fetch_policy;
mod http;
#[cfg(feature = "extract-native")]
mod native;
pub mod searxng;
pub mod tavily;
mod tool;
mod types;

pub use brave::BraveProvider;
pub use credentials::{WebSearchCredential, WebSearchCredentialState, WebSearchCredentials};
pub use exa::ExaProvider;
pub use extract_source::{
    ExtractedPageSink, ExtractedPageSinkError, StoredExtractedPage, MAX_EXTRACTED_PAGE_PASSAGES,
};
pub use fetch_policy::{
    admit_fetch_address, admit_fetch_url, FetchPolicyViolation, MAX_FETCH_URL_BYTES,
};
#[cfg(feature = "http")]
pub use http::ReqwestHttpClient;
pub use http::{
    HttpClient, HttpGetRequest, HttpRequest, HttpResponse, OutboundOrigin, MAX_HTTP_RESPONSE_BYTES,
};
#[cfg(feature = "extract-native")]
pub use native::{
    HostAddressResolver, NativeExtractError, NativeExtraction, NativeExtractor, PageFetchResponse,
    PageFetchTransport, ReqwestPageFetcher, TokioHostResolver, MAX_EXTRACT_CONTENT_CHARS,
    MAX_FETCH_REDIRECT_HOPS, MAX_FETCH_RESPONSE_BYTES, NATIVE_FETCH_USER_AGENT,
};
pub use searxng::{SearxngBaseUrl, SearxngProvider};
pub use tavily::TavilyProvider;
pub use tool::{
    extract_request_from_tool_arguments, request_from_tool_arguments, PageExtractor,
    WebExtractTool, WebSearchResolver, WebSearchResolverError, WebSearchTool, DEFAULT_MAX_RESULTS,
};
pub use types::{
    ExtractionMethod, SearchDomain, WebExtractFailure, WebExtractRequest, WebExtractResponse,
    WebSearchError, WebSearchProvider, WebSearchProviderKind, WebSearchRequest, WebSearchResponse,
    WebSearchResult, EXTRACT_TRUNCATION_MARKER, MAX_DOMAINS, MAX_EXTRACT_OUTPUT_BYTES,
    MAX_OUTPUT_BYTES, MAX_OUTPUT_CHARS, MAX_QUERY_CHARS, MAX_RESULTS, MAX_RESULT_CONTENT_CHARS,
    MAX_RESULT_SNIPPET_CHARS, MAX_RESULT_TITLE_CHARS, MAX_RESULT_URL_BYTES, MIN_EXTRACT_WORDS,
};
