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

## Current boundary

`openwave-server::web_search::resolve_provider` is the host-only construction
seam for a future approved execution worker. It is inert until that caller
invokes `search`; no route invokes it. No model-visible `web_search` tool or
sandbox tool loop exists yet. The next slice must explicitly attach this host
policy to a sandbox worker, define an outbound-domain policy, and preserve the
turn/checkpoint idempotency guarantees before search can be used by an agent.
