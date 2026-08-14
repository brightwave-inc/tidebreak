# Web search configuration

Tidebreak has a bounded web-search module in `tidebreak-server` (`crates/tidebreak-server/src/web_search`) for direct search and for
single-page extraction. The crate owns the provider-neutral request/result
contracts, HTTP adapters, and the foreground `WebSearchTool` and
`WebExtractTool`; the server supplies current host policy and credentials
through a resolver. Configuration alone performs no outbound request.

## Backends

| Backend | Needs | Search | Extract |
| --- | --- | --- | --- |
| Exa | paid API key | yes | yes |
| Tavily | paid API key | yes | yes |
| Brave | API key, free tier available | yes | no — routes to the native engine |
| SearXNG | a self-hosted instance URL, no key | yes | no — routes to the native engine |

Two of the four therefore work without paying anyone: Brave on its free tier,
and SearXNG on an instance the operator runs.

## Adapters are plain HTTP, and stay that way

Every adapter talks to its backend over plain HTTP through the crate's small
`HttpClient` seam. **No vendor SDK or client library is used, and none should be
added.**

The reason is that cargo features are resolved at compile time. An SDK for a
backend that most users never configure would still be compiled into every
shipped binary — its dependency tree, its own transport, its update cadence —
paid for by everyone who does not use it. An HTTP adapter reuses the client that
is already present and costs essentially nothing per backend. It also keeps
every backend under the same discipline: one bound origin, one timeout, no
redirects, no vendor-native JSON escaping into a normalized response.

If an SDK ever turns out to be genuinely unavoidable for a backend, it must sit
behind an off-by-default cargo feature so the default binary does not carry it,
and any UI-side equivalent must be a dynamic import so the default bundle does
not either. Adding one is a design decision to argue for in review, not a
convenience.

## Local API

The loopback API exposes a bearer-protected host policy at `/web-search`.

| Endpoint | Purpose |
| --- | --- |
| `GET /web-search` | Read the selected provider, timeout, the SearXNG instance URL, and whether the selection is usable. |
| `PUT /web-search` | Select `exa`, `tavily`, `brave`, `searxng`, or `null` to disable (clears the host provider and sets mode `off` so turns do not fall back to vendor search); optionally set `mode`, timeout, and the SearXNG instance URL. |
| `GET /web-search/credentials` | Read readiness for the fixed Exa, Tavily, and Brave key slots. |
| `PUT /web-search/credentials/{provider}` | Store a key for one of those slots; returns readiness only. |
| `DELETE /web-search/credentials/{provider}` | Delete that fixed provider key; returns readiness only. |

Example:

```json
{
  "provider": "exa",
  "timeout_ms": 20000
}
```

Timeouts must be between 1,000 and 60,000 milliseconds. There is no proxy URL,
arbitrary secret reference, or API-key field in this API, and the one address it
does accept — `searxng_base_url` — is described under
[SearXNG](#searxng-self-hosted). Every hosted adapter owns its fixed HTTPS
endpoints, disables redirects, and bounds request input and retained response
output. The host constructs the concrete HTTP client for the selected provider
only; that client is bound to exactly one origin and rejects any request whose
scheme or exact authority differs from it before dispatch, on both the `POST`
and `GET` verbs of the transport seam.

`GET /web-search` reports `has_credential` (a key is stored for the selected
provider) and `available` (a turn that routes host search here can invoke the
selected provider). `available` is false when mode is `off` or `vendor`, even
if a key is still stored — settings and the turn tool surface stay aligned.
Otherwise the two differ only for SearXNG, which has no key slot at all:
`has_credential` is always false there and `available` follows the instance
URL.

No response contains a credential. `/web-search/credentials` reports readiness
for every fixed key slot; SearXNG is absent from it, exactly as local execution
is absent from the code-execution credential list. Keys are read
from and written to the OS keychain through
`SecretProvider` under the fixed names `web_search.exa.api_key`,
`web_search.tavily.api_key`, and `web_search.brave.api_key`. Credential writes
reject empty values and values over 8 KiB. They never alter selection or
timeout policy. A missing key or disabled selection fails closed: there is no
provider to invoke.

## Brave Search

Brave is the search-only backend with a free tier, so it is the one a host can
turn on without a paid plan. It is a `GET` against the fixed
`https://api.search.brave.com/res/v1/web/search` with the key in the
`X-Subscription-Token` header — never in the query string, which is the part of
a `GET` that lands in proxy and server logs — and `Accept: application/json`.

| | Brave |
| --- | --- |
| Query | `q`, `count` (the request's `max_results`; the endpoint's own cap is 20) |
| Snippet markup | `text_decorations=false`, so `description` arrives as plain text instead of `<strong>`-wrapped matches that would have to be stripped |
| Response scope | `result_filter=web`, keeping the news, video, discussion, and infobox clusters out of the payload |
| Dates | `freshness=YYYY-MM-DDtoYYYY-MM-DD`, sent only when the request carries both ends, which is what a custom range requires |
| Domains | no include-domains parameter; `site:` operators are folded into `q` and returned results are then filtered by host |
| Result mapping | `url`, `title`, `description` → snippet, `page_age` → `published_at`. The sibling `age` is a relative phrase ("2 days ago") and carries no instant, so it is not read. There is no page text or relevance score in the response, so `content` and `score` stay empty rather than being synthesized. |

The domain filter is worth spelling out because it is the one place a backend
cannot express the request natively. The `site:` operators are a hint — whether
an upstream ranker honours them is not something this crate can promise — so
the filter is *also* applied to the returned results by host, matching the
domain itself or any subdomain of it. That is what actually holds the contract:
a domain-restricted call cannot return an off-domain page. When the operators
would push `q` past the endpoint's 400-character limit they are left off and the
result filter alone does the work.

Statuses map to the crate's typed errors: `401` is a rejected credential and
`429` is a rate limit. Brave documents no separate quota code — a spent monthly
allowance answers `429` as well — so `QuotaExhausted` has nothing to map from
here and no billing status is invented for it. Everything else stays a plain
HTTP status.

Brave publishes no page-extraction endpoint, so it does not implement the
extract contract and `web_extract` routes to the native engine (see
[Page extraction](#page-extraction)).

## SearXNG (self-hosted)

SearXNG is a metasearch engine the operator runs themselves. It is the backend
that needs no vendor account at all, and it is the only one that departs from
two of this crate's standing rules. Both departures are narrow and neither
loosens anything for the other providers.

### It carries no credential

Every other provider holds an API key in the OS keychain and is unusable until
that key is present. SearXNG has nothing to hold. Rather than making the key
optional — which would quietly turn "no key stored" into "usable" for providers
that do need one — credential resolution answers with three states:

- **Present** — the provider's fixed key is stored and non-empty.
- **Missing** — the provider requires a key and none is stored. The host fails
  closed and makes no request. This is unchanged for Exa, Tavily, and Brave.
- **Not required** — the provider takes no credential. Only SearXNG reaches it.

SearXNG has no keychain slot, does not appear in `/web-search/credentials`, and
cannot be addressed by `PUT`/`DELETE /web-search/credentials/{provider}` — the
same shape local execution already has in the code-execution surface.

### Its address is configuration

Every other provider pins a fixed `outbound_domain` so neither host settings nor
a model argument can redirect egress. A self-hosted instance has no fixed
address to pin, so `searxng_base_url` is the one address this surface accepts.
What keeps that safe:

- **It is host configuration only.** It is never a model argument, is not
  derivable from tool input, and no tool schema mentions it. The only way to set
  it is the authenticated local `PUT /web-search`.
- **It is validated where it is stored,** the same way the code-execution egress
  allowlist is: `http` or `https`, a real host, no userinfo, no query, no
  fragment, no relative path segments, bounded length. A malformed value is
  rejected at `PUT` time rather than silently widening where the transport may
  dial, and a malformed value already in the store makes the whole configuration
  fail closed to disabled on read. The canonical form is what gets stored, so
  there is one spelling of an instance URL everywhere.
- **The search endpoint is derived from it,** never supplied beside it: the base
  URL plus `/search`.
- **The transport is still bound to exactly one origin.** A configured origin
  goes through the same scheme/host/port check a fixed one does, so an instance
  at `http://localhost:8888` cannot reach `:8889`, another host, or the same
  host over a different scheme.

Loopback and private addresses are permitted here, because that is where
self-hosted instances live. This is a deliberately different trust class from
the URLs `web_extract` fetches: the operator typed this address into their own
settings, whereas `fetch_policy` governs addresses the model or a fetched page
chose. **`fetch_policy` itself is unchanged** — it still denies loopback,
private, link-local, metadata, CGNAT, and ULA space for model-supplied URLs, in
every IP encoding.

### Request and response

| | SearXNG |
| --- | --- |
| Request | `GET {base}/search` with `q`, `format=json`, `pageno=1`, and `Accept: application/json`. No credential header of any kind. |
| Domains | no domain-filter parameter; `site:` operators are folded into `q` and returned results are then filtered by host, exactly as for Brave |
| Dates | `time_range` is `day`/`month`/`year` only and cannot express the request's window, so none is sent and the window is applied to results that carry a date; undated results are kept, because most upstream engines report none and dropping them would empty an answer rather than narrow it |
| Result count | the API takes no count, so the request's `max_results` bound is applied to the results |
| Result mapping | `url`, `title`, `content` → snippet, `score`, `publishedDate` → `published_at` (read as UTC when it carries no offset). The `engine`/`engines` provenance fields are not carried: they would spend the output budget on a name a model cannot act on. |

The JSON output format is **off by default** in many deployments, and a public
instance usually leaves it off. That is why an instance that does not answer
with JSON gets its own typed failure rather than a generic invalid response: a
documented `403`, or a `200` carrying the HTML results page, both resolve to
"the instance did not return its JSON API", which names the repair. `429` is
the instance's own request limiter; everything else stays a plain HTTP status.

SearXNG is search-only, so `web_extract` routes to the native engine.

## Desktop setup

The desktop sidebar has a **Web search** panel for this same local API. It
shows whether the saved configuration is disabled, ready, or still missing what
the selected provider needs; offers a key field per fixed slot, so every
credentialed backend can hold a key at once and switching between them needs no
retyping; offers the SearXNG instance URL as its own field, with no key field
because there is no key; and lets the user pick which provider is active (or
disable search) and set the bounded timeout. Existing keys are never displayed
or read into the renderer. Saving writes every key the user typed before it
writes selection, so a provider cannot become active in a pass that failed to
store its key. Removing a key stays a separate action against a single slot;
clearing the instance URL field takes SearXNG out of service without touching
anything else.

## Current boundary

`tidebreak-server::web_search::resolve_provider` is the host-only construction
seam. It is inert until a caller invokes `search`; no route invokes it.
Foreground agents receive a Sensitive `web_search` tool. Each exact call is
persisted and parked on the durable approval gate before execution; renderer
copy describes only that the query and explicit filters will leave Tidebreak,
without exposing model-authored arguments. Approval resolves the provider from
current settings, so enabling, disabling, or changing providers takes effect
without rebuilding the registry. Cancelling the turn races and drops an
in-flight tool future, which aborts the underlying HTTP request.

## Page extraction

The Sensitive `web_extract` tool opens one exact public page URL and returns
its readable content — title, markdown content, word count, and a truncation
flag — bounded before it can reach a model context. The approval card shows the
whole URL, because the URL is the action: it is both what leaves the device and
where the request goes.

The page is also kept: see [Fetched pages as sources](#fetched-pages-as-sources)
below.

Routing is deterministic and derived from the configured provider, with no
heuristics and no escalation. The `WebSearchProvider` trait carries a
capability split (`supports_search` / `supports_extract`); extraction goes to
the configured provider exactly when it implements the extract contract, and to
the built-in native engine otherwise — including when the provider is
search-only or when no provider is configured at all. Extraction therefore
works with zero web-search configuration.

Exa and Tavily both implement the extract contract, so a host with either
selected extracts through the vendor and falls back to native. Brave is
search-only and never receives an extraction request. They reach
`https://api.exa.ai/contents` and `https://api.tavily.com/extract`, on the same
fixed authority their search calls use, with the key as a bearer token.

| | Exa | Tavily |
| --- | --- | --- |
| Request | one URL, `text` always sent explicitly, bounded at the source with `text.maxCharacters` (10,000) | one URL, `extract_depth: basic`, `format: markdown` |
| Freshness | `maxAgeHours: 24`, the supported replacement for the deprecated `livecrawl` selector | vendor default |
| Title | taken from the response | none in the response; reported empty rather than invented |
| Per-URL failure | HTTP 200 with the URL absent from `results` and an error in `statuses` | HTTP 200 with the URL in `failed_results` |

Both endpoints answer HTTP 200 when the single requested URL failed, so the
adapters read the per-URL outcome first and match results by URL key rather
than array position. A missing, failed, or too-thin result is a typed
`PageNotExtracted`, never an extraction with nothing in it. The vendor's own
error tag or prose is not deserialized at all: it would only ever be forwarded,
and no vendor string belongs in a model context.

Request-level statuses are mapped to typed errors so the routing can act on
them: `401` is a rejected credential, Exa's `402` and Tavily's nonstandard `432`
(plan allowance) and `433` (pay-as-you-go balance) are an exhausted quota,
`429` is a rate limit, and everything else stays a plain HTTP status. The
timeout is the host's clamped `timeout_ms` for both search and extraction;
neither adapter takes a timeout of its own and no extract call carries one in
its payload.

The native engine (the crate's `extract-native` feature) admits a URL through
a strict fetch policy — https only, no userinfo, default port, and a denied
network list covering loopback, private, link-local/metadata, CGNAT, and ULA
space in every IP encoding — then follows redirects manually, re-admitting
every hop with fresh DNS resolution and pinning each connection to the vetted
addresses. Fetches carry no cookies or ambient credentials, stream under a hard
byte cap, gate on a textual content-type allowlist, and reduce the page with a
readability pass to bounded markdown.

Failure is layered and never silent. A vendor extract failure — quota, rate
limit, timeout, a page the vendor could not read — falls back to the native
engine for that request, with no vendor diagnostic crossing either way. The one
exception is a rejected API key: that is host configuration rather than a
property of the page, it will reject every later call, and it is the same key
web search uses, so falling back would hide a broken configuration behind
quietly degraded extraction forever. It surfaces as the typed
configuration-required failure instead, which the desktop renders as the
settings card that repairs it. A native failure returns a closed, actionable
reason ("the page returned HTTP 404", "no readable content could be extracted
from the page") with no transport or vendor diagnostics attached. When no
extraction path exists at all, the tool returns that same typed
configuration-required failure. Every successful extraction is stamped with its
`extraction_method` (`native` or the provider name) so degraded extraction
stays visible downstream.

## Fetched pages as sources

An extracted page is stored immediately as an ordinary canonical-text source in
the conversation that fetched it. Its identity is derived from the conversation
and final page URL, so re-reading a page replaces that source instead of
accumulating duplicates.

The row retains the final URL, sanitized title, fetch time, `text/markdown`
media type, and extracted text. It has no original blob, parser fingerprint,
source regions, or background work. `read_document` can open the stored text
later, and the model may include its document id and lightweight locator in the
response's **Sources** row or put the page URL directly in prose.

Page content is untrusted throughout. Titles and content from every engine are
stripped of control characters, zero-width marks, and bidirectional overrides
in `WebExtractResponse::new`. If the page cannot be stored, the content is
still returned and the result says plainly that it cannot be attached to the
response as a stored source.

The server's sandbox checkpoint executor invokes the same resolver and strict
argument decoder only after it has claimed a persisted `web_search`
checkpoint. It then revalidates the exact lease, cancellation state, and run
deadline with the database clock immediately before calling the provider. It
keeps its local execution timeout below that database-derived lease budget and
resolves one immutable receipt.

Malformed arguments, disabled selection, missing credentials, provider failure,
timeout, and invalid output resolve a bounded redacted
failure receipt rather than leaving a sandbox waiting. The depth-one sandbox
model loop may advertise only this fixed `web_search` schema, at most once and
only when two model steps remain. It parks the immutable call before any
egress; when the receipt resolves, its next claim rebuilds the same
`ToolUse`/`ToolResult` pair and may finalize. Sandbox checkpoint authority does
not cross into the foreground registry: that path uses the ordinary durable
tool-call and approval state machine, while recursive agents remain impossible.
The concrete transport is bound to exactly one origin outside model-controlled
arguments: the fixed HTTPS domain for the hosted providers (`api.exa.ai`,
`api.tavily.com`, `api.search.brave.com`) and the validated configured origin
for a self-hosted SearXNG instance.
