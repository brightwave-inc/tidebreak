//! Web search: bounded provider adapters, fetch admission, native extraction,
//! and the host-owned configuration surface that selects and credentials them.
//!
//! Provider adapters deliberately do not register a model tool, load credentials,
//! or make network calls at construction time. The host resolves a provider key
//! from [`tidebreak_core::SecretProvider`], constructs an adapter, and calls
//! [`WebSearchProvider::search`]. Concrete HTTP clients bind each adapter to its
//! provider's exact HTTPS domain before any tool or worker can use it.
//!
//! Host configuration and the foreground/sandbox tool wiring live in [`host`].

mod brave;
mod credentials;
mod exa;
mod extract_source;
mod fetch_policy;
mod host;
mod http;
mod model_provider;
mod native;
mod searxng;
mod tavily;
mod tool;
mod types;

pub use brave::BraveProvider;
pub use credentials::{WebSearchCredential, WebSearchCredentialState, WebSearchCredentials};
pub use exa::ExaProvider;
pub use extract_source::{ExtractedPageSink, ExtractedPageSinkError, StoredExtractedPage};
pub use fetch_policy::{
    admit_fetch_address, admit_fetch_url, FetchPolicyViolation, MAX_FETCH_URL_BYTES,
};
pub use host::{
    config_info, credential_provider, credentials_info, delete_credential, read_config,
    resolve_provider, resolve_turn_web_search, update_config, write_credential, WebSearchConfig,
    WebSearchConfigInfo, WebSearchConfigUpdate, WebSearchCredentialReadiness,
    WebSearchCredentialsInfo, WebSearchMode, DEFAULT_TIMEOUT_MS, MAX_TIMEOUT_MS, MIN_TIMEOUT_MS,
};
pub use host::{foreground_extract_tool, foreground_tool};
pub use http::{
    HttpClient, HttpGetRequest, HttpRequest, HttpResponse, OutboundOrigin, ReqwestHttpClient,
    MAX_HTTP_RESPONSE_BYTES,
};
pub use model_provider::{ModelProviderSearch, SearchModel};
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
