# Web search configuration

OpenWave has a bounded `openwave-web-search` library for direct Exa and Tavily
search and for single-page extraction. The crate owns the provider-neutral
request/result contracts, HTTP adapters, and the foreground `WebSearchTool` and
`WebExtractTool`; the server supplies current host policy and credentials
through a resolver. Configuration alone performs no outbound request.

## Local API

The loopback API exposes a bearer-protected host policy at `/web-search`.

| Endpoint | Purpose |
| --- | --- |
| `GET /web-search` | Read the selected provider, timeout, and whether its fixed key is available. |
| `PUT /web-search` | Select `exa`, `tavily`, or `null` to disable; optionally set the timeout. |
| `GET /web-search/credentials` | Read readiness for the fixed Exa and Tavily key slots. |
| `PUT /web-search/credentials/{provider}` | Store a key for `exa` or `tavily`; returns readiness only. |
| `DELETE /web-search/credentials/{provider}` | Delete that fixed provider key; returns readiness only. |

Example:

```json
{
  "provider": "exa",
  "timeout_ms": 20000
}
```

Timeouts must be between 1,000 and 60,000 milliseconds. There is no endpoint,
proxy URL, arbitrary secret reference, or API-key field in this API. The Exa
and Tavily adapters own their fixed HTTPS endpoints, disable redirects, and
bound request input and retained response output. The host constructs the
concrete HTTP client for the selected provider only; that client rejects any
request whose scheme or exact authority differs from the provider's fixed API
domain before dispatch.

No response contains a credential. `/web-search` reports only
`has_credential` for the currently selected provider, while
`/web-search/credentials` reports readiness for both fixed slots. Keys are read
from and written to the OS keychain through
`SecretProvider` under the fixed names `web_search.exa.api_key` and
`web_search.tavily.api_key`. Credential writes reject empty values and values
over 8 KiB. They never alter selection or timeout policy. A missing key or
disabled selection fails closed: there is no provider to invoke.

## Desktop setup

The desktop sidebar has a **Web search** panel for this same local API. It
shows whether the saved configuration is disabled, ready, or missing the
selected provider's key; offers a key field per fixed slot, so Exa and Tavily
can both hold a key and switching between them needs no retyping; and lets the
user pick which provider is active (or disable search) and set the bounded
timeout. Existing keys are never displayed or read into the renderer. Saving
writes every key the user typed before it writes selection, so a provider cannot
become active in a pass that failed to store its key. Removing a key stays a
separate action against a single slot.

## Current boundary

`openwave-server::web_search::resolve_provider` is the host-only construction
seam. It is inert until a caller invokes `search`; no route invokes it.
Foreground agents receive a Sensitive `web_search` tool. Each exact call is
persisted and parked on the durable approval gate before execution; renderer
copy describes only that the query and explicit filters will leave OpenWave,
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

Routing is deterministic and derived from the configured provider, with no
heuristics and no escalation. The `WebSearchProvider` trait carries a
capability split (`supports_search` / `supports_extract`); extraction goes to
the configured provider exactly when it implements the extract contract, and to
the built-in native engine otherwise — including when the provider is
search-only or when no provider is configured at all. Extraction therefore
works with zero web-search configuration.

Exa and Tavily both implement the extract contract, so a host with either
selected extracts through the vendor and falls back to native. They reach
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
The concrete transport enforces an exact HTTPS
outbound-domain policy (`api.exa.ai` for Exa and `api.tavily.com` for Tavily)
outside model-controlled arguments.
