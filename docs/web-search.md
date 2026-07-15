# Web search configuration

OpenWave has a bounded `openwave-web-search` library for direct Exa and Tavily
search. It is intentionally separate from the model tool registry and agent
workers. At this stage, configuring web search does not give an agent network
access and does not cause any outbound request.

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
bound request input and retained response output.

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
selected provider's key; lets the user choose Exa or Tavily (or disable search),
set the bounded timeout, and save, replace, or remove a key. Existing keys are
never displayed or read into the renderer. Saving a key and saving provider
selection are deliberately separate actions, matching the API boundary above.

## Current boundary

`openwave-server::web_search::resolve_provider` is the host-only construction
seam. It is inert until a caller invokes `search`; no route invokes it. The
server's sandbox checkpoint executor may invoke it only after it has claimed a
persisted `web_search` checkpoint. It resolves host settings and credentials,
then revalidates the exact lease, cancellation state, and run deadline with the
database clock immediately before calling the provider. It keeps its local
execution timeout below that database-derived lease budget and resolves one
immutable receipt.

Malformed arguments, disabled selection, missing credentials, provider failure,
timeout, and invalid output resolve a bounded redacted
failure receipt rather than leaving a sandbox waiting. There is still no
model-visible `web_search` tool or sandbox tool loop: without a durable
checkpoint the executor does not construct a provider or make an outbound
request. A later slice must add model-loop checkpoint emission and an explicit
outbound-domain policy before search is model-usable.
