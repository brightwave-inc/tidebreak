//! Searching through the chat's own model provider.
//!
//! Every other backend in this module is a search engine with an address and a
//! key. This one has neither: it hands one query to the provider the chat is
//! already talking to, on that provider's own credential or subscription, and
//! reads back what the answer cited.
//!
//! The request is deliberately tool-free. That is not an implementation detail
//! — it is the whole reason this path is allowed to exist. Both the OpenAI and
//! the Gemini adapters refuse to declare a hosted search alongside the host's
//! own function tools, because neither endpoint can bound how many searches one
//! agent turn would then spend (`openai.rs`'s profile guard,
//! `gemini.rs`'s `declares_search_grounding`). A sub-request has no such
//! problem: the host issues exactly one, never continues it, and counts its own
//! calls. The budget moves off the wire and onto this host, where it can
//! actually be enforced.
//!
//! What comes back is thinner than a search engine's answer. Both providers
//! report citations as title and URL with no page excerpt, so results carry no
//! snippet and the agent follows up with `web_extract` for anything it needs to
//! read. Domain filters are honoured the same way Brave and SearXNG honour
//! them: passed as query operators, then enforced here on the results.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use tidebreak_core::provider::VendorWebSearch;
use tidebreak_core::{
    ChatMessage, ChatRequest, ModelProvider, PromptCacheMode, ProviderEvent, ProviderId,
    ReasoningEffort, Role,
};

use super::types::{domain_scoped_query, result_within_domains, result_within_published_window};
use crate::web_search::{
    WebSearchError, WebSearchProvider, WebSearchProviderKind, WebSearchRequest, WebSearchResponse,
    WebSearchResult, MAX_QUERY_CHARS,
};

/// The tool name both adapters report a provider-executed search under.
const VENDOR_WEB_SEARCH_TOOL: &str = "web_search";

/// Searches the provider may run inside the one sub-request.
///
/// Anthropic is the only route that reads this as a wire budget; OpenAI has no
/// field for it and Gemini ignores it. It is declared honestly anyway, because
/// the number that actually bounds this path is the host's own call count, and
/// one call is what one host tool call buys.
const SEARCHES_PER_SUBREQUEST: u32 = 1;

/// Upper bound on tokens one search sub-request generates.
///
/// The answer's prose is discarded — only its citations are read — so this
/// needs to cover a short summary and nothing more. It still has to leave a
/// reasoning model room to think before it cites, which is why it is not
/// smaller.
const SEARCH_MAX_OUTPUT_TOKENS: u32 = 2_048;

/// What the sub-request asks for.
///
/// It asks for prose rather than a list because the citations, not the text,
/// are the product: a model told to answer in a structured shape tends to stop
/// citing. The instruction not to answer from memory is what stops a model
/// treating a "latest" question as something it already knows — the failure
/// this whole path exists to prevent.
const SEARCH_SYSTEM_PROMPT: &str = "\
You are a search tool. Search the web for what the user asks about and answer \
in one short paragraph, citing every source you used. Never answer from prior \
knowledge without searching: your knowledge is stale and the caller wants what \
the web says now.";

/// The model one search sub-request runs on.
#[derive(Clone, Debug)]
pub struct SearchModel {
    /// Provider route, as the router's own hint.
    pub provider: Option<ProviderId>,
    /// Provider model identifier.
    pub model: String,
    /// Whether the route takes a reasoning-model request shape.
    pub reasoning_model: bool,
    /// Effort levels the model accepts, ascending; empty when it exposes none.
    pub reasoning_efforts: Vec<ReasoningEffort>,
}

impl SearchModel {
    /// The cheapest effort this model will accept.
    ///
    /// A search sub-request is a lookup, not a problem: it reads results and
    /// cites them. Running it at the chat's own effort would spend reasoning
    /// tokens on a call whose prose is discarded. `clamp_to` degrades to the
    /// nearest supported level, so a model whose floor is higher than `None`
    /// still gets a level it accepts.
    fn effort(&self) -> Option<ReasoningEffort> {
        ReasoningEffort::None.clamp_to(&self.reasoning_efforts)
    }
}

/// Searches by asking the chat's own model provider.
pub struct ModelProviderSearch {
    providers: Arc<dyn ModelProvider>,
    model: SearchModel,
}

impl ModelProviderSearch {
    #[must_use]
    pub fn new(providers: Arc<dyn ModelProvider>, model: SearchModel) -> Self {
        Self { providers, model }
    }

    fn request(&self, search: &WebSearchRequest) -> ChatRequest {
        ChatRequest {
            provider: self.model.provider.clone(),
            model: self.model.model.clone(),
            reasoning_model: self.model.reasoning_model,
            system: Some(SEARCH_SYSTEM_PROMPT.to_owned()),
            messages: vec![ChatMessage::text(
                Role::User,
                domain_scoped_query(&search.query, &search.domains, MAX_QUERY_CHARS),
            )],
            // The empty tool list is what makes this shape legal on both
            // adapters. Adding a tool here would put the request back into the
            // unbounded agent-turn shape they refuse.
            tools: Vec::new(),
            max_tokens: Some(SEARCH_MAX_OUTPUT_TOKENS),
            temperature: None,
            reasoning_effort: self.model.effort(),
            vendor_web_search: Some(VendorWebSearch {
                max_uses: SEARCHES_PER_SUBREQUEST,
            }),
            // One call, one prompt nothing else re-sends: a cache write here
            // would be a premium paid for an entry that expires unread.
            prompt_cache: PromptCacheMode::OneShot,
            ..Default::default()
        }
    }
}

#[async_trait]
impl WebSearchProvider for ModelProviderSearch {
    fn kind(&self) -> WebSearchProviderKind {
        WebSearchProviderKind::ModelProvider
    }

    async fn search(&self, request: WebSearchRequest) -> Result<WebSearchResponse, WebSearchError> {
        request.validate()?;
        let mut stream = self
            .providers
            .stream(self.request(&request))
            .await
            .map_err(|error| WebSearchError::Transport(error.to_string()))?;

        let mut results = Vec::new();
        while let Some(event) = stream.next().await {
            match event {
                ProviderEvent::ProviderExecutedToolCall {
                    name,
                    output,
                    is_error,
                    ..
                } if name == VENDOR_WEB_SEARCH_TOOL => {
                    if is_error {
                        continue;
                    }
                    results.extend(cited_results(&output, &request));
                    // One sub-request can run more than one search: a reasoning
                    // model browses with `open_page` and `find_in_page` as well
                    // as `search`, and each finished item arrives as its own
                    // call. The caller asked for `max_results` in total, not per
                    // item, so the cap belongs here rather than inside the
                    // per-call mapping.
                    results.truncate(request.max_results);
                }
                // A refusal is the model declining to answer, which is not the
                // same as a search that found nothing: reporting it as an empty
                // result set would tell the agent the web is silent on the
                // subject.
                ProviderEvent::Refusal { .. } => {
                    return Err(WebSearchError::Transport("the model refused".into()))
                }
                ProviderEvent::Failed { .. } => {
                    return Err(WebSearchError::Transport(
                        "the search sub-request failed".into(),
                    ))
                }
                // Everything else is the prose answer, its reasoning, and its
                // accounting. Only the citations are the product here.
                _ => {}
            }
        }

        Ok(WebSearchResponse::new(self.kind(), results))
    }
}

/// Read one provider-executed search's normalized output into host results.
///
/// Both adapters already emit `{ provider, results: [{ url, title, snippet }] }`
/// — the shape the host's own `web_search` tool produces — so this reads that
/// contract rather than either provider's wire format. A row that fails the
/// host's own validation is dropped rather than failing the search: the
/// remaining citations are still worth returning.
fn cited_results(output: &Value, request: &WebSearchRequest) -> Vec<WebSearchResult> {
    let Some(rows) = output.get("results").and_then(Value::as_array) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| {
            let url = row.get("url").and_then(Value::as_str)?;
            let title = row.get("title").and_then(Value::as_str).unwrap_or_default();
            let snippet = row
                .get("snippet")
                .and_then(Value::as_str)
                .unwrap_or_default();
            WebSearchResult::new(
                url,
                title,
                snippet,
                None,
                None,
                // Neither provider dates a citation. `result_within_published_window`
                // keeps undated results, so a publication window narrows nothing
                // here rather than emptying the answer.
                None,
                None,
                std::collections::BTreeMap::new(),
            )
            .ok()
        })
        .filter(|result| result_within_domains(&result.url, &request.domains))
        .filter(|result| {
            result_within_published_window(
                result.published_at,
                request.start_published_at,
                request.end_published_at,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web_search::SearchDomain;
    use futures::stream::{self, BoxStream};
    use serde_json::json;

    /// A provider that answers with a scripted run of hosted-search calls.
    struct ScriptedSearches(Vec<Value>);

    #[async_trait]
    impl ModelProvider for ScriptedSearches {
        fn id(&self) -> ProviderId {
            ProviderId::new("scripted")
        }

        async fn stream(
            &self,
            _req: ChatRequest,
        ) -> tidebreak_core::Result<BoxStream<'static, ProviderEvent>> {
            let events: Vec<ProviderEvent> = self
                .0
                .iter()
                .map(|output| ProviderEvent::ProviderExecutedToolCall {
                    name: VENDOR_WEB_SEARCH_TOOL.to_owned(),
                    input: json!({"query": "q"}),
                    output: output.clone(),
                    is_error: false,
                    replay: None,
                })
                .collect();
            Ok(stream::iter(events).boxed())
        }
    }

    fn search_over(outputs: Vec<Value>) -> ModelProviderSearch {
        ModelProviderSearch::new(
            Arc::new(ScriptedSearches(outputs)),
            SearchModel {
                provider: Some(ProviderId::new("openai")),
                model: "gpt-5.6-sol".into(),
                reasoning_model: true,
                reasoning_efforts: vec![ReasoningEffort::Low],
            },
        )
    }

    fn cited(urls: &[&str]) -> Value {
        json!({
            "provider": "openai",
            "results": urls
                .iter()
                .map(|url| json!({"url": url, "title": "T", "snippet": ""}))
                .collect::<Vec<_>>(),
        })
    }

    /// `max_results` is a total, not a per-call allowance.
    ///
    /// One sub-request can finish several hosted searches — a reasoning model
    /// browses with `open_page` and `find_in_page` as well as `search`, and each
    /// arrives as its own call. Capping inside the per-call mapping would let a
    /// three-result request answer with nine, which no other backend does and
    /// which the caller pays for in context.
    #[tokio::test]
    async fn several_searches_in_one_sub_request_still_honour_max_results() {
        let search = search_over(vec![
            cited(&["https://a.test/1", "https://a.test/2"]),
            cited(&["https://b.test/1", "https://b.test/2"]),
            cited(&["https://c.test/1"]),
        ]);

        let response = search
            .search(WebSearchRequest::new("news", 3).unwrap())
            .await
            .unwrap();

        assert_eq!(response.results.len(), 3);
        assert_eq!(response.results[0].url, "https://a.test/1");
        assert_eq!(response.results[2].url, "https://b.test/1");
    }

    fn request(query: &str) -> WebSearchRequest {
        WebSearchRequest::new(query, 5).unwrap()
    }

    #[test]
    fn citations_become_host_results() {
        let output = json!({
            "provider": "openai",
            "results": [
                {"url": "https://example.com/a", "title": "A", "snippet": ""},
                {"url": "https://example.org/b", "title": "B", "snippet": "excerpt"},
            ],
        });

        let results = cited_results(&output, &request("news"));

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].url, "https://example.com/a");
        assert_eq!(results[0].title, "A");
        // Absent excerpts stay absent rather than becoming invented text.
        assert_eq!(results[0].snippet, "");
        assert_eq!(results[1].snippet, "excerpt");
    }

    /// The filter has to be real here for the same reason it is real for Brave:
    /// the `site:` operators in the query are a hint the provider may ignore.
    #[test]
    fn results_outside_the_requested_domains_are_dropped() {
        let output = json!({
            "provider": "gemini",
            "results": [
                {"url": "https://example.com/a", "title": "A", "snippet": ""},
                {"url": "https://docs.example.com/b", "title": "B", "snippet": ""},
                {"url": "https://elsewhere.test/c", "title": "C", "snippet": ""},
            ],
        });
        let request = request("news")
            .with_domains(vec![SearchDomain::parse("example.com").unwrap()])
            .unwrap();

        let results = cited_results(&output, &request);

        // The host itself and its subdomain, but not the unrelated origin.
        assert_eq!(results.len(), 2);
        assert_eq!(results[1].url, "https://docs.example.com/b");
    }

    /// A search that cited nothing ran and found nothing. It is not a failure,
    /// and it must not become one: the agent's next move differs entirely.
    #[test]
    fn an_answer_that_cited_nothing_yields_no_results() {
        let empty = json!({"provider": "openai", "results": []});
        assert!(cited_results(&empty, &request("news")).is_empty());

        let malformed = json!({"provider": "openai"});
        assert!(cited_results(&malformed, &request("news")).is_empty());
    }

    /// The tool-free shape is the contract that both provider adapters check.
    /// A tool appearing on this request would put it back into the shape they
    /// refuse, and the failure would surface as a dead search rather than as
    /// anything that names this call.
    #[test]
    fn the_sub_request_carries_no_tools_and_asks_for_one_search() {
        let providers: Arc<dyn ModelProvider> = Arc::new(crate::provider::UnconfiguredProvider);
        let search = ModelProviderSearch::new(
            providers,
            SearchModel {
                provider: Some(ProviderId::new("openai")),
                model: "gpt-5.6-sol".into(),
                reasoning_model: true,
                reasoning_efforts: vec![ReasoningEffort::Low, ReasoningEffort::High],
            },
        );

        let built = search.request(&request("who won"));

        assert!(built.tools.is_empty());
        assert_eq!(
            built.vendor_web_search,
            Some(VendorWebSearch { max_uses: 1 })
        );
        // Lowest level the model accepts, not the chat's own.
        assert_eq!(built.reasoning_effort, Some(ReasoningEffort::Low));
    }
}
