//! Native page extraction: fetch one admitted URL and reduce the page to
//! bounded, readable text.
//!
//! The extractor follows redirects manually so that every hop — not just the
//! first URL — passes the full [`crate::fetch_policy`] admission check with a
//! fresh DNS resolution, and each connection is pinned to the exact addresses
//! that were vetted. That re-vetting is the defense against DNS rebinding and
//! redirect-based server-side request forgery: a page may not launder a fetch
//! toward loopback, private, link-local, or metadata address space through a
//! redirect or a second DNS answer. Fetches carry no cookies and no ambient
//! credentials, announce a distinct honest user agent, and retain a bounded
//! body of an allow-listed textual content type only.
//!
//! Extraction itself never returns junk to a caller: a page whose readable
//! content falls below a small word floor — including script-only shells that
//! ask the visitor to enable JavaScript — resolves the typed
//! [`NativeExtractError::NoReadableContent`] so a caller can distinguish
//! "extracted" from "nothing there".

use std::net::IpAddr;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;
use url::{Host, Url};

use crate::fetch_policy::{admit_fetch_address, admit_fetch_url, FetchPolicyViolation};

/// Largest response body retained for one native page fetch.
pub const MAX_FETCH_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// Redirect hops followed before a fetch is refused.
pub const MAX_FETCH_REDIRECT_HOPS: usize = 5;
/// Maximum Unicode scalar values of extracted content returned to a caller,
/// including the truncation marker when one is inserted.
pub const MAX_EXTRACT_CONTENT_CHARS: usize = 24_000;
/// Below this many extracted words a page has no readable content.
pub const MIN_EXTRACT_WORDS: usize = 20;
/// The honest, distinct user agent every native page fetch announces.
pub const NATIVE_FETCH_USER_AGENT: &str =
    "OpenWavePageExtractor/1.0 (+https://github.com/brightwave-inc/openwave)";

/// Marker inserted between the head and tail of over-budget content.
const TRUNCATION_MARKER: &str = "\n\n[... content truncated ...]\n\n";
/// Most document elements the readability pass will parse.
const MAX_PARSE_ELEMENTS: usize = 200_000;
/// Longest content-type echoed back in an error.
const MAX_CONTENT_TYPE_ECHO_CHARS: usize = 100;
/// Word ceiling under which JavaScript-shell boilerplate disqualifies a page.
const MAX_SHELL_BOILERPLATE_WORDS: usize = 150;

/// Readable content extracted from one fetched page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeExtraction {
    /// Final fetched URL after redirects, fragment stripped.
    pub url: String,
    /// Page title; empty when the page did not provide one.
    pub title: String,
    /// Main content as markdown (plain text for a `text/*` page), bounded by
    /// [`MAX_EXTRACT_CONTENT_CHARS`] with head-and-tail truncation.
    pub content: String,
    /// Words in the full extraction, counted before any truncation.
    pub word_count: usize,
    /// Whether `content` was truncated to fit the character budget.
    pub truncated: bool,
}

/// Why a native extraction failed. Closed and typed so a caller can act on
/// the reason without parsing prose, and so no vendor or transport payload
/// leaks through it.
#[derive(Debug, Error)]
pub enum NativeExtractError {
    #[error("page fetch violates the URL admission policy: {0}")]
    PolicyViolation(#[from] FetchPolicyViolation),
    #[error("page host did not resolve: {0}")]
    DnsResolution(String),
    #[error("page fetch failed: {0}")]
    Fetch(String),
    #[error("page returned HTTP {0}")]
    HttpStatus(u16),
    #[error("page redirected more than {MAX_FETCH_REDIRECT_HOPS} times")]
    TooManyRedirects,
    #[error("page redirect location is missing or invalid")]
    InvalidRedirect,
    #[error("page response exceeded the byte limit")]
    ResponseTooLarge,
    #[error("page content type {0:?} is not extractable")]
    UnsupportedContentType(String),
    #[error("page has no readable content")]
    NoReadableContent,
}

/// One vetted response from the page transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageFetchResponse {
    pub status: u16,
    /// Raw `Content-Type` header value, when present.
    pub content_type: Option<String>,
    /// Raw `Location` header value, when present.
    pub location: Option<String>,
    pub body: Vec<u8>,
}

impl PageFetchResponse {
    /// Reject a custom transport's oversized body before it is parsed.
    pub fn ensure_bounded(&self) -> Result<(), NativeExtractError> {
        if self.body.len() > MAX_FETCH_RESPONSE_BYTES {
            return Err(NativeExtractError::ResponseTooLarge);
        }
        Ok(())
    }
}

/// One admitted GET. Implementations must connect only to `addresses` (the
/// vetted resolution of the URL's host), never follow redirects themselves,
/// send no cookies or ambient credentials, and cap the retained body at
/// [`MAX_FETCH_RESPONSE_BYTES`].
#[async_trait]
pub trait PageFetchTransport: Send + Sync {
    async fn get(
        &self,
        url: &Url,
        addresses: &[IpAddr],
        timeout: Duration,
    ) -> Result<PageFetchResponse, NativeExtractError>;
}

/// Fresh DNS resolution for one host. The extractor calls this on every
/// redirect hop and vets every returned address before any connection.
#[async_trait]
pub trait HostAddressResolver: Send + Sync {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, NativeExtractError>;
}

/// The native extraction engine, backed by an injected transport and resolver.
///
/// It has no default constructor because the host must explicitly decide the
/// transport policy and the request timeout.
#[derive(Clone, Debug)]
pub struct NativeExtractor<T, R> {
    transport: T,
    resolver: R,
    timeout: Duration,
}

impl<T, R> NativeExtractor<T, R> {
    pub fn new(transport: T, resolver: R, timeout: Duration) -> Result<Self, NativeExtractError> {
        if timeout.is_zero() {
            return Err(NativeExtractError::Fetch(
                "page fetch timeout must be greater than zero".into(),
            ));
        }
        Ok(Self {
            transport,
            resolver,
            timeout,
        })
    }
}

impl<T: PageFetchTransport, R: HostAddressResolver> NativeExtractor<T, R> {
    /// Fetch `url` safely and extract its readable content.
    pub async fn extract(&self, url: &str) -> Result<NativeExtraction, NativeExtractError> {
        let (final_url, response) = self.fetch_vetted(url).await?;
        let media_type = extractable_media_type(response.content_type.as_deref())?;
        let body = String::from_utf8_lossy(&response.body);
        let (title, content) = match media_type {
            PageMediaType::Html => readable_article(&body, &final_url)?,
            PageMediaType::Text => (String::new(), body.trim().to_owned()),
        };
        let word_count = count_words(&content);
        if word_count < MIN_EXTRACT_WORDS {
            return Err(NativeExtractError::NoReadableContent);
        }
        let (content, truncated) = truncate_head_tail(&content, MAX_EXTRACT_CONTENT_CHARS);
        Ok(NativeExtraction {
            url: final_url.into(),
            title: crate::types::truncate(title.trim(), crate::MAX_RESULT_TITLE_CHARS),
            content,
            word_count,
            truncated,
        })
    }

    /// Follow redirects manually, re-admitting every hop's URL and freshly
    /// resolved addresses, until a non-redirect response arrives.
    async fn fetch_vetted(
        &self,
        url: &str,
    ) -> Result<(Url, PageFetchResponse), NativeExtractError> {
        let mut current = admit_fetch_url(url)?;
        for _ in 0..=MAX_FETCH_REDIRECT_HOPS {
            let addresses = self.vetted_addresses(&current).await?;
            let response = self
                .transport
                .get(&current, &addresses, self.timeout)
                .await?;
            if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                let location = response
                    .location
                    .as_deref()
                    .ok_or(NativeExtractError::InvalidRedirect)?;
                let next = current
                    .join(location)
                    .map_err(|_| NativeExtractError::InvalidRedirect)?;
                current = admit_fetch_url(next.as_str())?;
                continue;
            }
            if !(200..300).contains(&response.status) {
                return Err(NativeExtractError::HttpStatus(response.status));
            }
            response.ensure_bounded()?;
            return Ok((current, response));
        }
        Err(NativeExtractError::TooManyRedirects)
    }

    /// Resolve the URL's host and admit every address, so the transport can
    /// pin its connection to exactly what was vetted.
    async fn vetted_addresses(&self, url: &Url) -> Result<Vec<IpAddr>, NativeExtractError> {
        let addresses = match url.host() {
            // `admit_fetch_url` already vetted a literal, but hops are cheap
            // to re-check and the policy is the boundary.
            Some(Host::Ipv4(address)) => vec![IpAddr::V4(address)],
            Some(Host::Ipv6(address)) => vec![IpAddr::V6(address)],
            Some(Host::Domain(host)) => self.resolver.resolve(host).await?,
            None => return Err(FetchPolicyViolation::MissingHost.into()),
        };
        if addresses.is_empty() {
            return Err(NativeExtractError::DnsResolution(
                "host resolved to no addresses".into(),
            ));
        }
        for address in &addresses {
            admit_fetch_address(*address)?;
        }
        Ok(addresses)
    }
}

enum PageMediaType {
    Html,
    Text,
}

/// Allow-list the response content type; anything else is a typed refusal,
/// never a guess.
fn extractable_media_type(value: Option<&str>) -> Result<PageMediaType, NativeExtractError> {
    let raw = value.unwrap_or("");
    let essence = raw
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match essence.as_str() {
        "text/html" | "application/xhtml+xml" => Ok(PageMediaType::Html),
        "text/plain" | "text/markdown" => Ok(PageMediaType::Text),
        _ => Err(NativeExtractError::UnsupportedContentType(
            crate::types::truncate(&essence, MAX_CONTENT_TYPE_ECHO_CHARS),
        )),
    }
}

/// Run the readability pass over fetched HTML and return `(title, markdown)`.
fn readable_article(html: &str, url: &Url) -> Result<(String, String), NativeExtractError> {
    let config = dom_smoothie::Config {
        max_elements_to_parse: MAX_PARSE_ELEMENTS,
        text_mode: dom_smoothie::TextMode::Markdown,
        ..dom_smoothie::Config::default()
    };
    let article = dom_smoothie::Readability::new(html, Some(url.as_str()), Some(config))
        .and_then(|mut readability| readability.parse())
        .map_err(|_| NativeExtractError::NoReadableContent)?;
    let content = article.text_content.trim().to_owned();
    if looks_like_script_shell(&content) {
        return Err(NativeExtractError::NoReadableContent);
    }
    Ok((article.title, content))
}

/// A near-empty page that asks the visitor to enable JavaScript is an
/// application shell, not content.
fn looks_like_script_shell(extracted: &str) -> bool {
    if count_words(extracted) > MAX_SHELL_BOILERPLATE_WORDS {
        return false;
    }
    let lower = extracted.to_lowercase();
    [
        "enable javascript",
        "javascript is required",
        "javascript is disabled",
        "turn on javascript",
        "javascript to run this app",
    ]
    .iter()
    .any(|tell| lower.contains(tell))
}

/// Words that carry content: whitespace-separated tokens with at least one
/// alphanumeric character, so markdown punctuation does not inflate the count.
fn count_words(value: &str) -> usize {
    value
        .split_whitespace()
        .filter(|token| token.chars().any(char::is_alphanumeric))
        .count()
}

/// Keep the head and tail of over-budget content with an explicit marker in
/// between; the result never exceeds `budget` characters.
fn truncate_head_tail(value: &str, budget: usize) -> (String, bool) {
    let total = value.chars().count();
    if total <= budget {
        return (value.to_owned(), false);
    }
    let marker_chars = TRUNCATION_MARKER.chars().count();
    let keep = budget.saturating_sub(marker_chars);
    let head_chars = keep * 2 / 3;
    let tail_chars = keep - head_chars;
    let head: String = value.chars().take(head_chars).collect();
    let tail: String = value.chars().skip(total - tail_chars).collect();
    (format!("{head}{TRUNCATION_MARKER}{tail}"), true)
}

/// Page transport that builds one pinned `reqwest` client per request.
///
/// Building per request is what lets the connection be pinned: the vetted
/// addresses are installed as the host's only resolution, so the connect
/// cannot re-resolve to something the policy never saw. Redirects stay
/// disabled — the extractor's loop is the only redirect follower — and the
/// client holds no cookie store and attaches no credentials.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReqwestPageFetcher;

#[async_trait]
impl PageFetchTransport for ReqwestPageFetcher {
    async fn get(
        &self,
        url: &Url,
        addresses: &[IpAddr],
        timeout: Duration,
    ) -> Result<PageFetchResponse, NativeExtractError> {
        use futures::StreamExt;

        if timeout.is_zero() {
            return Err(NativeExtractError::Fetch(
                "page fetch timeout must be greater than zero".into(),
            ));
        }
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .user_agent(NATIVE_FETCH_USER_AGENT)
            .connect_timeout(timeout.min(Duration::from_secs(10)))
            .timeout(timeout);
        if let Some(Host::Domain(host)) = url.host() {
            let pinned: Vec<std::net::SocketAddr> = addresses
                .iter()
                .map(|address| std::net::SocketAddr::new(*address, 443))
                .collect();
            builder = builder.resolve_to_addrs(host, &pinned);
        }
        let client = builder
            .build()
            .map_err(|error| NativeExtractError::Fetch(error.to_string()))?;
        let response = client
            .get(url.as_str())
            .header(
                reqwest::header::ACCEPT,
                "text/html, application/xhtml+xml, text/plain;q=0.9, text/markdown;q=0.9",
            )
            .send()
            .await
            .map_err(|error| NativeExtractError::Fetch(error.to_string()))?;
        let status = response.status().as_u16();
        let header = |name: reqwest::header::HeaderName| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(ToOwned::to_owned)
        };
        let content_type = header(reqwest::header::CONTENT_TYPE);
        let location = header(reqwest::header::LOCATION);
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| NativeExtractError::Fetch(error.to_string()))?;
            if body.len().saturating_add(chunk.len()) > MAX_FETCH_RESPONSE_BYTES {
                return Err(NativeExtractError::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(PageFetchResponse {
            status,
            content_type,
            location,
            body,
        })
    }
}

/// Host resolver backed by the operating system's resolver.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioHostResolver;

#[async_trait]
impl HostAddressResolver for TokioHostResolver {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, NativeExtractError> {
        let resolved = tokio::net::lookup_host((host, 443))
            .await
            .map_err(|error| NativeExtractError::DnsResolution(error.to_string()))?;
        let mut addresses = Vec::new();
        for address in resolved.map(|socket| socket.ip()) {
            if !addresses.contains(&address) {
                addresses.push(address);
            }
        }
        Ok(addresses)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::Mutex;

    use super::*;

    const PUBLIC_V4: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(93, 184, 216, 34));
    const PRIVATE_V4: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 5));

    const ARTICLE_HTML: &str = r#"<!doctype html>
<html><head><title>Rust Ownership Explained</title>
<style>body { color: red; }</style>
<script>window.telemetry = "do-not-extract-this-token";</script>
</head><body>
<article>
<h1>Rust Ownership Explained</h1>
<p>Ownership is the discipline that lets Rust manage memory without a garbage
collector. Every value has a single owner, and when the owner goes out of
scope the value is dropped deterministically.</p>
<p>Borrowing lets other code read or mutate a value without taking ownership.
The compiler enforces that mutable borrows are exclusive, which rules out data
races at compile time rather than at runtime.</p>
<p>Lifetimes describe how long references remain valid. Most of the time the
compiler infers them, and explicit annotations only appear where the
relationships between references are ambiguous.</p>
</article>
</body></html>"#;

    const SHELL_HTML: &str = r#"<!doctype html>
<html><head><title>App</title>
<script src="/static/bundle.js"></script>
<script>window.__STATE__ = {"a":1,"b":2,"c":3,"d":4,"e":5,"f":6,"g":7};</script>
</head><body>
<div id="root"><p>You need to enable JavaScript to run this app.</p></div>
</body></html>"#;

    struct ScriptedTransport {
        responses: Mutex<VecDeque<PageFetchResponse>>,
        seen: Mutex<Vec<(String, Vec<IpAddr>)>>,
    }

    impl ScriptedTransport {
        fn new(responses: Vec<PageFetchResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl PageFetchTransport for ScriptedTransport {
        async fn get(
            &self,
            url: &Url,
            addresses: &[IpAddr],
            _timeout: Duration,
        ) -> Result<PageFetchResponse, NativeExtractError> {
            self.seen
                .lock()
                .unwrap()
                .push((url.to_string(), addresses.to_vec()));
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| NativeExtractError::Fetch("no scripted response".into()))
        }
    }

    struct StaticResolver(HashMap<&'static str, Vec<IpAddr>>);

    #[async_trait]
    impl HostAddressResolver for StaticResolver {
        async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, NativeExtractError> {
            self.0
                .get(host)
                .cloned()
                .ok_or_else(|| NativeExtractError::DnsResolution(format!("unknown host {host}")))
        }
    }

    fn html_response(status: u16, body: &str) -> PageFetchResponse {
        PageFetchResponse {
            status,
            content_type: Some("text/html; charset=utf-8".into()),
            location: None,
            body: body.as_bytes().to_vec(),
        }
    }

    fn redirect_response(location: &str) -> PageFetchResponse {
        PageFetchResponse {
            status: 302,
            content_type: None,
            location: Some(location.into()),
            body: Vec::new(),
        }
    }

    fn build_extractor(
        responses: Vec<PageFetchResponse>,
        hosts: &[(&'static str, Vec<IpAddr>)],
    ) -> NativeExtractor<ScriptedTransport, StaticResolver> {
        NativeExtractor::new(
            ScriptedTransport::new(responses),
            StaticResolver(hosts.iter().cloned().collect()),
            Duration::from_secs(5),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn extracts_readable_markdown_through_a_vetted_redirect() {
        let extractor = build_extractor(
            vec![
                redirect_response("/articles/ownership"),
                html_response(200, ARTICLE_HTML),
            ],
            &[("example.com", vec![PUBLIC_V4])],
        );
        let extraction = extractor
            .extract("https://example.com/start#fragment")
            .await
            .unwrap();

        assert_eq!(extraction.url, "https://example.com/articles/ownership");
        assert_eq!(extraction.title, "Rust Ownership Explained");
        assert!(extraction.content.contains("single owner"));
        assert!(extraction.content.contains("Lifetimes"));
        assert!(!extraction.content.contains("do-not-extract-this-token"));
        assert!(!extraction.content.contains("color: red"));
        assert!(extraction.word_count >= MIN_EXTRACT_WORDS);
        assert!(!extraction.truncated);

        // Both hops were dialed with the vetted address, fragment stripped.
        let seen = extractor.transport.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].0, "https://example.com/start");
        assert_eq!(seen[0].1, vec![PUBLIC_V4]);
        assert_eq!(seen[1].0, "https://example.com/articles/ownership");
    }

    #[tokio::test]
    async fn redirect_hops_toward_denied_space_are_refused_before_any_fetch() {
        // A redirect to a host that resolves to RFC 1918 space must fail the
        // hop's fresh resolution vetting without a second transport call.
        let extractor = build_extractor(
            vec![
                redirect_response("https://internal.example/admin"),
                html_response(200, ARTICLE_HTML),
            ],
            &[
                ("example.com", vec![PUBLIC_V4]),
                ("internal.example", vec![PRIVATE_V4]),
            ],
        );
        let error = extractor
            .extract("https://example.com/start")
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            NativeExtractError::PolicyViolation(FetchPolicyViolation::DeniedAddress(_))
        ));
        assert_eq!(extractor.transport.seen.lock().unwrap().len(), 1);

        // A redirect straight to a denied IP literal is refused by URL
        // admission alone.
        let extractor = extractor_with_imds_redirect();
        let error = extractor
            .extract("https://example.com/start")
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            NativeExtractError::PolicyViolation(FetchPolicyViolation::DeniedAddress(_))
        ));
        assert_eq!(extractor.transport.seen.lock().unwrap().len(), 1);
    }

    fn extractor_with_imds_redirect() -> NativeExtractor<ScriptedTransport, StaticResolver> {
        build_extractor(
            vec![redirect_response(
                "https://169.254.169.254/latest/meta-data/",
            )],
            &[("example.com", vec![PUBLIC_V4])],
        )
    }

    #[tokio::test]
    async fn refuses_endless_redirects_and_non_success_statuses() {
        let hops = (0..=MAX_FETCH_REDIRECT_HOPS + 1)
            .map(|hop| redirect_response(&format!("/hop/{hop}")))
            .collect();
        let extractor = build_extractor(hops, &[("example.com", vec![PUBLIC_V4])]);
        let error = extractor
            .extract("https://example.com/start")
            .await
            .unwrap_err();
        assert!(matches!(error, NativeExtractError::TooManyRedirects));

        let extractor = build_extractor(
            vec![html_response(404, "missing")],
            &[("example.com", vec![PUBLIC_V4])],
        );
        assert!(matches!(
            extractor
                .extract("https://example.com/gone")
                .await
                .unwrap_err(),
            NativeExtractError::HttpStatus(404)
        ));
    }

    #[tokio::test]
    async fn refuses_unsupported_content_types_and_javascript_shells() {
        let mut response = html_response(200, "{\"not\": \"extractable\"}");
        response.content_type = Some("application/json".into());
        let extractor = build_extractor(vec![response], &[("example.com", vec![PUBLIC_V4])]);
        assert!(matches!(
            extractor
                .extract("https://example.com/api")
                .await
                .unwrap_err(),
            NativeExtractError::UnsupportedContentType(essence) if essence == "application/json"
        ));

        let extractor = build_extractor(
            vec![html_response(200, SHELL_HTML)],
            &[("example.com", vec![PUBLIC_V4])],
        );
        assert!(matches!(
            extractor
                .extract("https://example.com/app")
                .await
                .unwrap_err(),
            NativeExtractError::NoReadableContent
        ));
    }

    #[tokio::test]
    async fn over_budget_content_keeps_head_and_tail_with_a_marker() {
        let head_words = "alpha ".repeat(6_000);
        let tail_words = "omega ".repeat(6_000);
        let body = format!("{head_words}{tail_words}");
        let response = PageFetchResponse {
            status: 200,
            content_type: Some("text/plain".into()),
            location: None,
            body: body.into_bytes(),
        };
        let extractor = build_extractor(vec![response], &[("example.com", vec![PUBLIC_V4])]);
        let extraction = extractor
            .extract("https://example.com/big.txt")
            .await
            .unwrap();

        assert!(extraction.truncated);
        assert!(extraction.content.chars().count() <= MAX_EXTRACT_CONTENT_CHARS);
        assert!(extraction.content.contains(TRUNCATION_MARKER.trim()));
        assert!(extraction.content.starts_with("alpha"));
        assert!(extraction.content.ends_with("omega") || extraction.content.ends_with("omega "));
        assert_eq!(extraction.word_count, 12_000);
    }
}
