use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use thiserror::Error;
use tidebreak_core::{
    ApprovalClass, ChatId, ResultEntry, ResultEntryKind, Tool, ToolCtx, ToolErrorCategory,
    ToolOutput, ToolSpec, WebExtractArgs, WebSearchArgs,
};
use url::Url;

use super::extract_source::{extracted_page_result, uncitable_page_result, ExtractedPageSink};
use crate::web_search::{
    SearchDomain, WebExtractFailure, WebExtractRequest, WebExtractResponse, WebSearchError,
    WebSearchProvider, WebSearchRequest, WebSearchResult, MAX_EXTRACT_OUTPUT_BYTES,
    MAX_OUTPUT_BYTES,
};

/// Default result count when a model omits `max_results`.
pub const DEFAULT_MAX_RESULTS: usize = tidebreak_core::DEFAULT_WEB_SEARCH_RESULTS;

/// Opaque failure to resolve current host policy or credentials.
///
/// Configuration and secret-provider diagnostics must not enter model context.
#[derive(Debug, Clone, Copy, Error)]
#[error("web search resolver is unavailable")]
pub struct WebSearchResolverError;

/// Resolve the provider selected by current host policy without performing
/// egress. Implementations may load settings and credentials on every call so
/// configuration changes take effect without rebuilding the tool registry.
///
/// The chat is part of the question, not context for logging: one backend
/// searches through the chat's own model provider, so which provider answers
/// depends on which model the chat is on. `None` means the caller cannot name a
/// chat, which resolves to the configured engine or to nothing.
#[async_trait]
pub trait WebSearchResolver: Send + Sync {
    async fn resolve(
        &self,
        chat: Option<ChatId>,
    ) -> Result<Option<Arc<dyn WebSearchProvider>>, WebSearchResolverError>;
}

/// Decode and validate the one canonical model-facing argument shape.
///
/// This is shared by the ordinary foreground tool and the durable sandbox
/// checkpoint worker so both execution paths enforce identical bounds.
pub fn request_from_tool_arguments(arguments: Value) -> Result<WebSearchRequest, WebSearchError> {
    let arguments: WebSearchArgs = serde_json::from_value(arguments)
        .map_err(|_| WebSearchError::InvalidRequest("invalid tool arguments".into()))?;
    let domains = arguments
        .domains
        .into_iter()
        .map(SearchDomain::parse)
        .collect::<Result<Vec<_>, _>>()?;
    WebSearchRequest::new(arguments.query, arguments.max_results)?
        .with_domains(domains)?
        .with_published_between(arguments.start_published_at, arguments.end_published_at)
}

/// Provider-backed foreground web-search tool.
///
/// It is inert until an approved call reaches [`Tool::execute`], then resolves
/// current host configuration and performs one bounded provider request.
pub struct WebSearchTool {
    resolver: Arc<dyn WebSearchResolver>,
}

impl WebSearchTool {
    #[must_use]
    pub fn new(resolver: Arc<dyn WebSearchResolver>) -> Self {
        Self { resolver }
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn spec(&self) -> ToolSpec {
        tidebreak_core::web_search_tool_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> tidebreak_core::Result<ToolOutput> {
        let request = match request_from_tool_arguments(args) {
            Ok(request) => request,
            Err(_) => return Ok(ToolOutput::error("Web search arguments are invalid.")),
        };
        let provider = match self.resolver.resolve(Some(ctx.chat_id)).await {
            Ok(Some(provider)) => provider,
            Ok(None) => {
                return Ok(ToolOutput::failed(
                    ToolErrorCategory::ConfigurationRequired,
                    "Web search is not configured for this host.",
                ))
            }
            Err(_) => return Ok(ToolOutput::error("Web search could not complete.")),
        };
        let response = match provider.search(request).await {
            Ok(response) => response,
            Err(_) => return Ok(ToolOutput::error("Web search could not complete.")),
        };
        let result = match serde_json::to_string(&response) {
            Ok(result) if result.len() <= MAX_OUTPUT_BYTES => result,
            Ok(_) | Err(_) => {
                return Ok(ToolOutput::error(
                    "Web search returned an invalid response.",
                ))
            }
        };
        let entries = response.results.iter().map(page_entry).collect();
        Ok(ToolOutput::text(result).with_entries(entries))
    }
}

/// One bounded page extraction, independent of which engine performs it.
///
/// This is the native engine's seam into the tool: object-safe, so the tool
/// carries neither the engine's transport generics nor its cargo feature. The
/// vendor path does not pass through here — providers extract through
/// [`WebSearchProvider::extract`].
#[async_trait]
pub trait PageExtractor: Send + Sync {
    async fn extract_page(
        &self,
        request: &WebExtractRequest,
    ) -> Result<WebExtractResponse, WebExtractFailure>;
}

/// Decode and validate the one canonical model-facing extract argument shape,
/// running the full fetch admission policy before anything can egress.
pub fn extract_request_from_tool_arguments(
    arguments: Value,
) -> Result<WebExtractRequest, WebSearchError> {
    let arguments: WebExtractArgs = serde_json::from_value(arguments)
        .map_err(|_| WebSearchError::InvalidRequest("invalid tool arguments".into()))?;
    WebExtractRequest::new(arguments.url)
}

/// Foreground single-page extraction tool.
///
/// Routing is deterministic and derived from the configured provider: an
/// extract-capable provider receives the request, and a search-only or absent
/// provider routes to the native engine. A vendor failure falls back to the
/// native engine for that request, except a rejected credential, which is host
/// configuration and surfaces; a native failure returns a closed, actionable
/// reason. Nothing degrades silently — every success is stamped with its
/// extraction method.
///
/// A host that supplies a [`ExtractedPageSink`] additionally makes each fetched
/// page a source of the conversation, so a claim drawn from it can be cited,
/// identified, and reopened like a claim drawn from an imported file. A
/// host without one still extracts; its pages are simply not citable, and the
/// result says so.
pub struct WebExtractTool {
    resolver: Arc<dyn WebSearchResolver>,
    native: Option<Arc<dyn PageExtractor>>,
    sink: Option<Arc<dyn ExtractedPageSink>>,
}

impl WebExtractTool {
    #[must_use]
    pub fn new(
        resolver: Arc<dyn WebSearchResolver>,
        native: Option<Arc<dyn PageExtractor>>,
    ) -> Self {
        Self {
            resolver,
            native,
            sink: None,
        }
    }

    /// Keep every extracted page as a citable conversation source.
    #[must_use]
    pub fn with_page_sink(mut self, sink: Arc<dyn ExtractedPageSink>) -> Self {
        self.sink = Some(sink);
        self
    }

    /// Store one extraction and build the citable result, or say plainly that
    /// it could not be stored.
    ///
    /// A storage failure does not fail the call: the fetch already happened and
    /// its content is still worth reading. What it must not do is leave the
    /// model believing it can cite a page nobody kept.
    async fn citable_output(&self, ctx: &ToolCtx, response: &WebExtractResponse) -> ToolOutput {
        let Some(sink) = &self.sink else {
            return extraction_output(response, uncitable_page_result(response));
        };
        let Ok(stored) = sink.store_page(ctx.chat_id, response, Utc::now()).await else {
            return extraction_output(response, uncitable_page_result(response));
        };
        let content = extracted_page_result(response, stored.document_id);
        extraction_output(response, content)
    }
}

#[async_trait]
impl Tool for WebExtractTool {
    fn spec(&self) -> ToolSpec {
        tidebreak_core::web_extract_tool_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> tidebreak_core::Result<ToolOutput> {
        let request = match extract_request_from_tool_arguments(args) {
            Ok(request) => request,
            // The admission reason is closed policy prose ("page URL scheme
            // must be https"), safe and useful for the model to act on.
            Err(WebSearchError::InvalidRequest(reason)) => {
                return Ok(ToolOutput::error(format!(
                    "Web page extraction arguments are invalid: {reason}."
                )))
            }
            Err(_) => {
                return Ok(ToolOutput::error(
                    "Web page extraction arguments are invalid.",
                ))
            }
        };
        let provider = match self.resolver.resolve(Some(ctx.chat_id)).await {
            Ok(provider) => provider,
            Err(_) => return Ok(ToolOutput::error("Web page extraction could not complete.")),
        };
        // Deterministic routing: the configured provider takes the request
        // exactly when it implements the extract contract. A vendor failure
        // (quota, rate limit, timeout, an unreadable page) falls back to the
        // native engine for this request; no vendor diagnostic crosses either
        // way.
        if let Some(provider) = provider.filter(|provider| provider.supports_extract()) {
            match provider.extract(request.clone()).await {
                Ok(response) => return Ok(self.citable_output(ctx, &response).await),
                // A rejected key is the one vendor failure that is about the
                // host and not about this page. It will reject the next call
                // and every call after it, and the same key is what web search
                // uses, so falling back would hide a broken configuration
                // behind quietly degraded extraction forever. Surface it as the
                // typed configuration failure the settings card repairs.
                Err(WebSearchError::CredentialRejected(_)) => {
                    return Ok(ToolOutput::failed(
                        ToolErrorCategory::ConfigurationRequired,
                        "The configured web-search provider rejected its API key.",
                    ))
                }
                Err(_) => {}
            }
        }
        let Some(native) = &self.native else {
            return Ok(ToolOutput::failed(
                ToolErrorCategory::ConfigurationRequired,
                "Web page extraction is not available on this host.",
            ));
        };
        match native.extract_page(&request).await {
            Ok(response) => Ok(self.citable_output(ctx, &response).await),
            Err(failure) => Ok(ToolOutput::error(format!(
                "Web page extraction failed: {failure}."
            ))),
        }
    }
}

/// One bounded extraction and its result-card row.
///
/// `result` is already bounded by construction — the response's own content
/// budget dominates the fixed framing around it — but the ceiling is asserted
/// here anyway, because it is the last point before the text enters a model
/// context.
fn extraction_output(response: &WebExtractResponse, result: String) -> ToolOutput {
    if result.len() > MAX_EXTRACT_OUTPUT_BYTES {
        return ToolOutput::error("Web page extraction returned an invalid response.");
    }
    let label = if response.title.is_empty() {
        response.url.as_str()
    } else {
        response.title.as_str()
    };
    let mut entry = ResultEntry::new(ResultEntryKind::Link, label).with_web_url(&response.url);
    if let Some(host) = Url::parse(&response.url).ok().and_then(|url| {
        url.host_str()
            .map(|host| host.trim_start_matches("www.").to_owned())
    }) {
        entry = entry.with_detail(host);
    }
    let meta = if response.truncated {
        format!("{} words · truncated", response.word_count)
    } else {
        format!("{} words", response.word_count)
    };
    ToolOutput::text(result).with_entries(vec![entry.with_meta(meta)])
}

/// One web result as a card row.
///
/// The host, not the whole URL: a column of rows is for telling results apart,
/// and forty characters of query string does that worse than "sec.gov" does.
/// A URL the parser rejects still gets a row — its title is the result.
fn page_entry(result: &WebSearchResult) -> ResultEntry {
    let entry = ResultEntry::new(ResultEntryKind::Link, &result.title).with_web_url(&result.url);
    match Url::parse(&result.url).ok().and_then(|url| {
        url.host_str()
            .map(|host| host.trim_start_matches("www.").to_owned())
    }) {
        Some(host) => entry.with_detail(host),
        None => entry,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tidebreak_core::{ChatId, Tool};

    use super::*;
    use crate::web_search::{WebSearchProviderKind, WebSearchResponse};

    struct FakeResolver {
        provider: Option<Arc<dyn WebSearchProvider>>,
        fail: bool,
    }

    #[async_trait]
    impl WebSearchResolver for FakeResolver {
        async fn resolve(
            &self,
            _chat: Option<ChatId>,
        ) -> Result<Option<Arc<dyn WebSearchProvider>>, WebSearchResolverError> {
            if self.fail {
                Err(WebSearchResolverError)
            } else {
                Ok(self.provider.clone())
            }
        }
    }

    struct FakeProvider {
        requests: Mutex<Vec<WebSearchRequest>>,
        response: Result<WebSearchResponse, WebSearchError>,
    }

    #[async_trait]
    impl WebSearchProvider for FakeProvider {
        fn kind(&self) -> WebSearchProviderKind {
            WebSearchProviderKind::Exa
        }

        async fn search(
            &self,
            request: WebSearchRequest,
        ) -> Result<WebSearchResponse, WebSearchError> {
            self.requests.lock().unwrap().push(request);
            match &self.response {
                Ok(response) => Ok(response.clone()),
                Err(_) => Err(WebSearchError::Transport(
                    "private provider diagnostic".into(),
                )),
            }
        }
    }

    fn context() -> ToolCtx {
        ToolCtx::without_private_scratch(ChatId::new(), None)
    }

    #[test]
    fn shared_typed_schema_preserves_request_bounds() {
        let spec = tidebreak_core::web_search_tool_spec();
        assert_eq!(spec.name, "web_search");
        assert_eq!(
            spec.input_schema["properties"]["query"]["maxLength"],
            crate::web_search::MAX_QUERY_CHARS
        );
        assert_eq!(
            spec.input_schema["properties"]["max_results"]["maximum"],
            crate::web_search::MAX_RESULTS
        );
        assert_eq!(
            spec.input_schema["properties"]["domains"]["maxItems"],
            crate::web_search::MAX_DOMAINS
        );
        assert_eq!(spec.input_schema["additionalProperties"], false);
    }

    #[test]
    fn arguments_are_strict_and_share_provider_validation() {
        let request = request_from_tool_arguments(serde_json::json!({
            "query": " current release ",
            "domains": ["Example.com"],
        }))
        .unwrap();
        assert_eq!(request.query, "current release");
        assert_eq!(request.max_results, DEFAULT_MAX_RESULTS);
        assert_eq!(request.domains[0].as_str(), "example.com");

        for invalid in [
            serde_json::json!({"query": "ok", "endpoint": "https://private.invalid"}),
            serde_json::json!({"query": "ok", "max_results": 0}),
            serde_json::json!({"query": "ok", "domains": ["*.example.com"]}),
            serde_json::json!({"query": ""}),
        ] {
            assert!(request_from_tool_arguments(invalid).is_err());
        }
    }

    #[tokio::test]
    async fn tool_is_sensitive_and_resolves_configuration_at_execution() {
        let tool = WebSearchTool::new(Arc::new(FakeResolver {
            provider: None,
            fail: false,
        }));
        assert_eq!(tool.approval_class(), ApprovalClass::Sensitive);

        let output = tool
            .execute(&context(), serde_json::json!({"query": "Tidebreak"}))
            .await
            .unwrap();
        assert!(output.is_error);
        assert_eq!(
            output.content,
            "Web search is not configured for this host."
        );
        assert_eq!(
            output.error_category,
            Some(ToolErrorCategory::ConfigurationRequired)
        );
    }

    #[tokio::test]
    async fn tool_returns_bounded_normalized_json_without_provider_diagnostics() {
        let provider = Arc::new(FakeProvider {
            requests: Mutex::new(Vec::new()),
            response: Ok(WebSearchResponse::new(
                WebSearchProviderKind::Exa,
                Vec::new(),
            )),
        });
        let tool = WebSearchTool::new(Arc::new(FakeResolver {
            provider: Some(provider.clone()),
            fail: false,
        }));

        let output = tool
            .execute(
                &context(),
                serde_json::json!({"query": "Tidebreak", "max_results": 3}),
            )
            .await
            .unwrap();
        assert!(!output.is_error);
        assert!(output.content.len() <= MAX_OUTPUT_BYTES);
        assert_eq!(
            serde_json::from_str::<Value>(&output.content).unwrap(),
            serde_json::json!({"provider": "exa", "results": []})
        );
        assert_eq!(provider.requests.lock().unwrap()[0].max_results, 3);

        let failing = WebSearchTool::new(Arc::new(FakeResolver {
            provider: Some(Arc::new(FakeProvider {
                requests: Mutex::new(Vec::new()),
                response: Err(WebSearchError::Transport("secret value".into())),
            })),
            fail: false,
        }));
        let output = failing
            .execute(&context(), serde_json::json!({"query": "Tidebreak"}))
            .await
            .unwrap();
        assert_eq!(output.content, "Web search could not complete.");
        assert!(!output.content.contains("secret"));
        assert!(!output.content.contains("private provider"));
    }

    struct StubNative {
        calls: Mutex<usize>,
    }

    impl StubNative {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(0),
            })
        }
    }

    #[async_trait]
    impl PageExtractor for StubNative {
        async fn extract_page(
            &self,
            request: &WebExtractRequest,
        ) -> Result<WebExtractResponse, WebExtractFailure> {
            *self.calls.lock().unwrap() += 1;
            WebExtractResponse::new(
                crate::web_search::ExtractionMethod::Native,
                request.url(),
                "Native title",
                "native content ".repeat(4),
                8,
                false,
            )
            .map_err(|_| WebExtractFailure::UrlNotAllowed)
        }
    }

    /// A provider that opts into the extract contract, unlike the search-only
    /// `FakeProvider` above.
    struct ExtractCapableProvider {
        fail_extract: bool,
    }

    /// A provider whose key the vendor rejects.
    struct KeyRejectingProvider;

    #[async_trait]
    impl WebSearchProvider for KeyRejectingProvider {
        fn kind(&self) -> WebSearchProviderKind {
            WebSearchProviderKind::Tavily
        }

        fn supports_extract(&self) -> bool {
            true
        }

        async fn search(
            &self,
            _request: WebSearchRequest,
        ) -> Result<WebSearchResponse, WebSearchError> {
            unreachable!("extraction must never call search")
        }

        async fn extract(
            &self,
            _request: WebExtractRequest,
        ) -> Result<WebExtractResponse, WebSearchError> {
            Err(WebSearchError::CredentialRejected(
                WebSearchProviderKind::Tavily,
            ))
        }
    }

    #[async_trait]
    impl WebSearchProvider for ExtractCapableProvider {
        fn kind(&self) -> WebSearchProviderKind {
            WebSearchProviderKind::Exa
        }

        fn supports_extract(&self) -> bool {
            true
        }

        async fn search(
            &self,
            _request: WebSearchRequest,
        ) -> Result<WebSearchResponse, WebSearchError> {
            unreachable!("extraction must never call search")
        }

        async fn extract(
            &self,
            request: WebExtractRequest,
        ) -> Result<WebExtractResponse, WebSearchError> {
            if self.fail_extract {
                return Err(WebSearchError::Transport(
                    "private vendor diagnostic".into(),
                ));
            }
            WebExtractResponse::new(
                crate::web_search::ExtractionMethod::Provider(WebSearchProviderKind::Exa),
                request.url(),
                "Vendor title",
                "vendor content",
                2,
                false,
            )
        }
    }

    fn extract_tool(
        provider: Option<Arc<dyn WebSearchProvider>>,
        native: Option<Arc<dyn PageExtractor>>,
    ) -> WebExtractTool {
        WebExtractTool::new(
            Arc::new(FakeResolver {
                provider,
                fail: false,
            }),
            native,
        )
    }

    const EXTRACT_ARGS: fn() -> Value =
        || serde_json::json!({"url": "https://example.com/article"});

    #[tokio::test]
    async fn extraction_routes_by_declared_capability_and_stamps_its_method() {
        // A search-only provider never receives the request: it routes native.
        let native = StubNative::new();
        let search_only = Arc::new(FakeProvider {
            requests: Mutex::new(Vec::new()),
            response: Ok(WebSearchResponse::new(
                WebSearchProviderKind::Exa,
                Vec::new(),
            )),
        });
        let tool = extract_tool(Some(search_only.clone()), Some(native.clone()));
        let output = tool.execute(&context(), EXTRACT_ARGS()).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("Extracted by: native"));
        assert_eq!(*native.calls.lock().unwrap(), 1);
        assert!(search_only.requests.lock().unwrap().is_empty());

        // An extract-capable provider takes the request and the native engine
        // stays idle; the stamp names the vendor.
        let native = StubNative::new();
        let tool = extract_tool(
            Some(Arc::new(ExtractCapableProvider {
                fail_extract: false,
            })),
            Some(native.clone()),
        );
        let output = tool.execute(&context(), EXTRACT_ARGS()).await.unwrap();
        assert!(output.content.contains("Extracted by: exa"));
        assert_eq!(*native.calls.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn vendor_extract_failure_falls_back_to_native_without_leaking() {
        let native = StubNative::new();
        let tool = extract_tool(
            Some(Arc::new(ExtractCapableProvider { fail_extract: true })),
            Some(native.clone()),
        );
        let output = tool.execute(&context(), EXTRACT_ARGS()).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("Extracted by: native"));
        assert_eq!(*native.calls.lock().unwrap(), 1);
        assert!(!output.content.contains("private vendor"));

        // A rejected key is the exception: it is host configuration, so it
        // surfaces for repair instead of degrading quietly on every future
        // call. The native engine is not consulted.
        let native = StubNative::new();
        let tool = extract_tool(Some(Arc::new(KeyRejectingProvider)), Some(native.clone()));
        let output = tool.execute(&context(), EXTRACT_ARGS()).await.unwrap();
        assert!(output.is_error);
        assert_eq!(
            output.error_category,
            Some(ToolErrorCategory::ConfigurationRequired)
        );
        assert_eq!(*native.calls.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn extraction_failures_are_typed_actionable_and_never_silent() {
        // No native engine and no extract-capable provider: a typed
        // configuration-required failure the UI can turn into a settings hint.
        let tool = extract_tool(None, None);
        let output = tool.execute(&context(), EXTRACT_ARGS()).await.unwrap();
        assert!(output.is_error);
        assert_eq!(
            output.error_category,
            Some(ToolErrorCategory::ConfigurationRequired)
        );

        // A native failure surfaces its closed reason, not a diagnostic.
        struct FailingNative;
        #[async_trait]
        impl PageExtractor for FailingNative {
            async fn extract_page(
                &self,
                _request: &WebExtractRequest,
            ) -> Result<WebExtractResponse, WebExtractFailure> {
                Err(WebExtractFailure::NoReadableContent)
            }
        }
        let tool = extract_tool(None, Some(Arc::new(FailingNative)));
        let output = tool.execute(&context(), EXTRACT_ARGS()).await.unwrap();
        assert!(output.is_error);
        assert_eq!(
            output.content,
            "Web page extraction failed: no readable content could be extracted from the page."
        );

        // Admission rejects before anything can resolve or egress, with the
        // policy's own closed reason.
        let output = tool
            .execute(
                &context(),
                serde_json::json!({"url": "http://example.com/article"}),
            )
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("scheme must be https"));
    }
}
