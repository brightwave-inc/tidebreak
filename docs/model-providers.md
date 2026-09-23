# Model providers and cross-provider replay

How Tidebreak treats multiple model providers, what mid-conversation switching
is for, and the rule that keeps that capability cheap.

## Why mid-conversation switching exists

A chat can move from Claude to Gemini to GPT and back without starting a new
conversation. That is not primarily a model-comparison toy. It pays for two
things a local-first agent app actually needs:

1. **Mid-task failover.** A rate limit or outage forty turns into an agent
   task must not strand the work until one vendor recovers. Failover onto a
   fresh chat is trivial; failover onto an ongoing one requires replaying a
   provider-neutral transcript to a different adapter.
2. **Tiered use inside one task.** A cheaper model for grinding, a stronger
   one for the hard step, without forking the accumulated context. The
   conversation journal is the asset; "switching models means starting over"
   is hostile to that.

The journal is the truth. Each request renders that journal into the selected
provider's wire shape. Most of that cost is already paid by persistence and
ordinary replay.

## Provider tiers

Not every provider is a full peer for every advanced feature. Treat that as
policy, not as unfinished work:

1. **Tier 1** — one, maybe two providers. Full capability: provider-executed
   tools, images, caching semantics, first-class testing. Design advanced
   features against these and ship when they work here.
2. **Tier 2** — OpenAI-compatible endpoints, Ollama, OpenRouter, and other
   partial routes. Chat completion, ordinary tool calls, failover. Advanced
   features are best-effort or absent, and capability flags say so honestly.
   Gemini's dormant vendor web-search path (`supports_vendor_web_search: false`
   until grounding can coexist with host tools) is the pattern — generalize it
   rather than lighting every feature everywhere. Ollama is a named local
   runtime on this tier: no key for a default daemon, custom model rows, and
   the same flatten-on-switch rule as every other compatible route. OpenRouter
   is a named hosted aggregator: a fixed endpoint, an API key, and custom
   model rows.

Refuse to invent per-provider special cases below the router. If a feature
cannot be expressed through the existing `ModelProvider` trait plus registry
capability flags, it is Tier-1-only for now — not a reason to widen the
abstraction.

The failure mode to avoid is not "we support multiple providers." It is
"we implied every feature works identically on every provider." Honest
capability flags make the cheap version possible: a provider may be
legitimately partial without lying to the user.

## Custom models and discovery

Every direct provider takes custom model rows beside its curated ones. Only
the Model Gateway does not, because its catalog comes from the gateway. A
custom row is the reader's claim about a model's contract, so validation holds
it to what the route can actually carry:

- Image input is allowed on every route, because every adapter sends images.
- Reasoning-effort levels are limited to the ones the provider's route sends
  (`ProviderKind::custom_reasoning_efforts`). Anthropic reasoning needs a
  Claude 4.6 or later id, the first generation the adapter sends adaptive
  thinking to.
- Structured output counts only where the provider's route enforces a schema
  (`ProviderKind::enforces_structured_output`).
- A custom id may not shadow a curated id of the same provider. When a catalog
  update curates an id a reader already added, the curated row wins in the
  catalog and in bare-id resolution. The provider list leaves the saved row
  out of `models` and names it in `replaced_by_built_in`, and the next save
  to that provider drops it, so the row never blocks a save.
- A configured row reads tolerantly, like every REST record, and a field
  added to it serializes only when it differs from its default. The update
  body still refuses a row key the server does not know.

Find models (`POST /providers/{kind}/models/discover`) reads the provider's
own model listing with the saved key, on the server. The response carries
model data only: a row that echoes the key is dropped, and an error names the
status but never repeats the provider's body. Discovery proposes rows; saving
them goes through the same validation as a row typed by hand.

## Flatten-on-switch

**Foreign provider-native artifacts degrade to plain content.** One rule,
applied uniformly, so the cost of *N* providers stays *O(N)*, not *O(N²)*.

When the next request's provider (or model route) is not the one that minted
a provider-coupled artifact:

| Artifact | Same route | Foreign route |
| --- | --- | --- |
| Reasoning / thinking blocks with signatures | Replay verbatim | Drop (sending none is always valid) |
| Provider-executed web search native blocks (e.g. Anthropic `encrypted_content`) | Replay verbatim | Flatten to cleartext titles/URLs (or equivalent host-shaped prose) — never invent a simulated native call |
| Vendor tool-call ids and cache prefixes | Keep as the adapter requires | Do not translate; the neutral journal already has the durable fact |

Sharing a wire protocol does not merge origins. A gateway can reuse the
Messages and Responses adapters, but its native artifacts still replay only for
the exact provider and model that minted them. Anthropic, OpenAI, Gemini, and
every other route receive the flattened or empty fallback.

Consequences:

- **No per-pair translation matrix.** Do not build Anthropic↔OpenAI↔Gemini
  converters for each new feature. Origin-gate native replay; everyone else
  gets the flattened form.
- **Quality asymmetry is accepted.** A switched-into model may see a slightly
  flatter history. That must not be hidden with fake native shapes, and it
  must never silently break (same refuse-don't-strip posture as unsupported
  image attachments).
- **No round-trip guarantee in UI or docs.** Switch is forward-looking: the
  journal is authoritative; each provider gets the best rendering that is
  cheap to produce. Nobody promises the Gemini leg is reconstructible in
  Claude's native format.

Existing machinery that already follows this:

- [`MessageReasoning::replayable_for`](../crates/tidebreak-core/src/provider.rs)
  — thinking blocks and signed Gemini function-call state only for the minting
  route. Foreign or legacy Gemini history uses Google's documented validator
  bypass instead of replaying another route's opaque signature.
- [`ProviderToolReplay::replayable_for`](../crates/tidebreak-core/src/provider.rs)
  — provider-executed native blocks only for the minting route.
- Host-shaped cleartext on `ProviderExecutedToolCall.output` — what foreign
  adapters and the UI always have.
- Registry flags such as `supports_vendor_web_search` — honest absence beats
  a half-working path.
- ChatGPT / Codex subscription auth — `ModelSpec::supports_chatgpt_auth`
  (and the catalog's per-model `available`) hide API-only OpenAI ids such as
  `gpt-5.4-nano` that Codex rejects, while API-key installs keep them.

## Checklist for a new provider-coupled feature

Before merging something that adds provider-native state to a turn:

1. Does the registry advertise the capability only where the path actually
   works?
2. Is native state origin-gated (same provider + model), with a cleartext or
   empty fallback for everyone else?
3. Is there **no** new pairwise translator between providers?
4. On an unsupported modality or missing capability, does the turn refuse or
   degrade visibly rather than strip silently?

If (2) or (3) feels hard, the feature is Tier-1-only until a flatten story
exists — not a reason to promise parity.
