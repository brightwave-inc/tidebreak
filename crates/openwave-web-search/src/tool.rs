use std::sync::Arc;

use async_trait::async_trait;
use openwave_core::{ApprovalClass, Tool, ToolCtx, ToolOutput, ToolSpec, WebSearchArgs};
use serde_json::Value;
use thiserror::Error;

use crate::{SearchDomain, WebSearchError, WebSearchProvider, WebSearchRequest, MAX_OUTPUT_BYTES};

/// Default result count when a model omits `max_results`.
pub const DEFAULT_MAX_RESULTS: usize = openwave_core::DEFAULT_WEB_SEARCH_RESULTS;

/// Opaque failure to resolve current host policy or credentials.
///
/// Configuration and secret-provider diagnostics must not enter model context.
#[derive(Debug, Clone, Copy, Error)]
#[error("web search resolver is unavailable")]
pub struct WebSearchResolverError;

/// Resolve the provider selected by current host policy without performing
/// egress. Implementations may load settings and credentials on every call so
/// configuration changes take effect without rebuilding the tool registry.
#[async_trait]
pub trait WebSearchResolver: Send + Sync {
    async fn resolve(&self) -> Result<Option<Arc<dyn WebSearchProvider>>, WebSearchResolverError>;
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
        openwave_core::web_search_tool_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> openwave_core::Result<ToolOutput> {
        let request = match request_from_tool_arguments(args) {
            Ok(request) => request,
            Err(_) => return Ok(ToolOutput::error("Web search arguments are invalid.")),
        };
        let provider = match self.resolver.resolve().await {
            Ok(Some(provider)) => provider,
            Ok(None) => {
                return Ok(ToolOutput::error(
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
        Ok(ToolOutput::text(result))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use openwave_core::{ChatId, Tool};

    use super::*;
    use crate::{WebSearchProviderKind, WebSearchResponse};

    struct FakeResolver {
        provider: Option<Arc<dyn WebSearchProvider>>,
        fail: bool,
    }

    #[async_trait]
    impl WebSearchResolver for FakeResolver {
        async fn resolve(
            &self,
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
        let spec = openwave_core::web_search_tool_spec();
        assert_eq!(spec.name, "web_search");
        assert_eq!(
            spec.input_schema["properties"]["query"]["maxLength"],
            crate::MAX_QUERY_CHARS
        );
        assert_eq!(
            spec.input_schema["properties"]["max_results"]["maximum"],
            crate::MAX_RESULTS
        );
        assert_eq!(
            spec.input_schema["properties"]["domains"]["maxItems"],
            crate::MAX_DOMAINS
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
            .execute(&context(), serde_json::json!({"query": "OpenWave"}))
            .await
            .unwrap();
        assert!(output.is_error);
        assert_eq!(
            output.content,
            "Web search is not configured for this host."
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
                serde_json::json!({"query": "OpenWave", "max_results": 3}),
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
            .execute(&context(), serde_json::json!({"query": "OpenWave"}))
            .await
            .unwrap();
        assert_eq!(output.content, "Web search could not complete.");
        assert!(!output.content.contains("secret"));
        assert!(!output.content.contains("private provider"));
    }
}
