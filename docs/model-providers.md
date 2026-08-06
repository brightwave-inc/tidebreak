# Model providers and cross-provider replay

How OpenWave treats multiple model providers, what mid-conversation switching
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
2. **Tier 2** — OpenAI-compatible endpoints and other partial routes. Chat
   completion, ordinary tool calls, failover. Advanced features are
   best-effort or absent, and capability flags say so honestly. Gemini's
   dormant vendor web-search path (`supports_vendor_web_search: false` until
   grounding can coexist with host tools) is the pattern — generalize it
   rather than lighting every feature everywhere.

Refuse to invent per-provider special cases below the router. If a feature
cannot be expressed through the existing `ModelProvider` trait plus registry
capability flags, it is Tier-1-only for now — not a reason to widen the
abstraction.

The failure mode to avoid is not "we support multiple providers." It is
"we implied every feature works identically on every provider." Honest
capability flags make the cheap version possible: a provider may be
legitimately partial without lying to the user.

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

- [`MessageReasoning::replayable_for`](../crates/openwave-core/src/provider.rs)
  — thinking blocks only for the minting route.
- [`ProviderToolReplay::replayable_for`](../crates/openwave-core/src/provider.rs)
  — provider-executed native blocks only for the minting route.
- Host-shaped cleartext on `ProviderExecutedToolCall.output` — what foreign
  adapters and the UI always have.
- Registry flags such as `supports_vendor_web_search` — honest absence beats
  a half-working path.

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
