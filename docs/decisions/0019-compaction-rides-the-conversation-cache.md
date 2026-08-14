# 19. Compaction rides the conversation's prompt cache

- Status: Accepted
- Date: 2026-08-13
- Owners: agent runtime
- Related: `crates/tidebreak-core/src/agent/context.rs`,
  `crates/tidebreak-core/src/compaction.rs`,
  `crates/tidebreak-router/src/anthropic.rs`, `docs/how-tidebreak-works.md`

## Context

Semantic compaction summarizes the old part of a chat so the model's view stops
growing. Until now it did that with a request of its own: the host's utility
model (Haiku by default), a dedicated system prompt, no tools, the raw prefix
refitted against the utility model's window, the prior checkpoint folded in as a
leading user/assistant pair, and a `response_format` JSON schema.

That request shares no bytes with the conversation. Prompt caching is an exact
byte-prefix match over the rendered request in the order tools → system →
messages, so a call that changes the model, the system prompt, and the tool
array reads nothing from the conversation's cache and pays full uncached price
for a whole copy of the transcript. The transcript in question is by definition
a large one: compaction only fires past 75% of the context window.

Two further facts about the Anthropic route, which is the one this codebase
tunes for, shaped the design:

- Cache entries exist only at `cache_control` breakpoints, and a lookup walks
  back at most 20 content blocks from a breakpoint to find a matching cached
  prefix. `mark_cacheable_transcript_tail` puts a breakpoint near the tail of
  every request and a lagging one 20 blocks behind it — the tail marker skips
  thinking blocks, so it can sit short of the last block — which in the ordinary
  case leaves the previous step's entry within reach of a request that only
  appends.
- The adapter implements `response_format` as an appended tool plus a forced
  `tool_choice`. Using it therefore rewrites the tool array (invalidating
  everything) and changes `tool_choice` (invalidating the messages cache). The
  same is true of any explicit `tool_choice`, at the messages level.

There is no read-only cache mode; a read costs about 0.1× input price and a
write about 1.25×. Entries expire after five minutes, and this codebase uses
that default TTL.

## Decision

Compaction is no longer a separate request. It is **the request the foreground
step was about to send, plus one trailing user message**.

Concretely, `maybe_create_context_checkpoint` receives a `RequestPrefix` —
messages, tools, image attachments, vendor-search budget — assembled by
`build_request_prefix`, the single path every step now uses. The checkpoint call
copies that prefix verbatim, appends `CONTEXT_CHECKPOINT_INSTRUCTION` as a user
message, and changes nothing else: same provider route, same model, same
`reasoning_model`/`reasoning_effort`/`temperature`, same system prompt, same
tool array, same hydrated images. `max_tokens` is the checkpoint's own, because
the output cap is not part of the hashed prefix. It is clamped down to the
chat's configured cap where there is one: a model that declares a lower output
ceiling rejects a larger request outright, and fail-open would swallow that into
a chat that silently never compacts.

It sends **no `response_format` and no `tool_choice`**. The checkpoint's V2
shape, its per-array limits, and the instruction not to call a tool are all
stated in the instruction text; `parse_and_canonicalize` still rejects anything
that does not conform, and the existing fail-open behavior on a tool call or a
parse failure is unchanged. The `attempted_boundary` fence still prevents a
second attempt within a turn.

Consequences of the "append only" rule that fall out of it:

- **No prior-checkpoint fold.** After a first compaction the projected
  checkpoint is already the first message of the fitted transcript, so folding
  happens naturally. An extra leading pair would break prefix identity anyway.
- **No separate summarization budget.** The prefix is whatever the step was
  going to send, already fitted and image-evicted.
- **The utility model is out of the compaction path entirely.** The role
  remains, and the chat titler and approval judge still use it;
  `AgentConfig::utility_model` is gone because nothing in the agent read it any
  more. `POST /chats/{id}/compact` no longer refuses with
  `compaction_utility_model_unavailable`.
- **The wrap-up step does not compact.** It constrains `tool_choice`, which
  costs the message cache a ride-along would have read, and a checkpoint written
  there cannot shorten a turn already writing its last answer.

### Prompt-cache mode

Caching is not free: an entry costs about 1.25× input price to write and pays
for itself only when a later request re-sends the same prefix. A conversation
does that on every step. A one-shot utility call — titling a chat, judging one
approval, the sandbox host-model proxy — sends a prompt nothing will ever
repeat, so every breakpoint it writes is billed at the premium and expires
unread. `PromptCacheMode::OneShot` says so on the request and adapters emit no
cache directives for it; `Conversation` is the default.

*Rejected: cache every request unconditionally.* It is one fewer field and one
fewer thing to get wrong, but it charges the write premium on calls that
structurally cannot read it back, which is a pure surcharge.

The standing warning: **compaction must remain `Conversation`.** `OneShot` reads
as the natural choice for a maintenance call, and setting it there suppresses
the very breakpoints the checkpoint request exists to read — the entire saving
this record describes vanishes with no test failing and no visible symptom. The
whole-struct parity assertion below is what catches it.

### The overlap question

The request now carries the protected tail as well as the prefix being
summarized, so the checkpoint can restate content that also stays raw after the
boundary. That is accepted rather than engineered away. The instruction tells
the model the newest messages stay in context verbatim and to spend its room on
durable state, and adds that repeating a little of the tail is fine while
dropping an old decision is not. A checkpoint is capped at 12 KiB; modest
overlap inside that budget is cheaper than any mechanism for preventing it, and
`select_compaction_boundary` is unchanged, so the durable anchor still comes
from the same message-id machinery it always did.

### What the summarizer can see

The prefix handed to the model is the *fitted* transcript, not the raw one.
Because the compaction threshold (0.75 × window) and the level-0 message budget
(0.75 × window − system − tools) are computed from the same number, deterministic
reduction is essentially always already active at the moment compaction fires,
so the oldest messages reach the summarizer floor-truncated. This is deliberate:
the fitted view is exactly what the model can still see, so a checkpoint that
compacts over content the fitted view has already dropped forfeits nothing that
was still reaching the model. The old design gave the summarizer its own budget
over the raw prefix and could read text the foreground could not; that fidelity
is what is traded for the cache.

### Wrap-up step

Separately but for the same reason: the wrap-up call after `max_steps` used to
send `tools: Vec::new()` to guarantee termination. Tools render at byte zero, so
that request shared no prefix with anything cached — full price on the largest
transcript of the turn. It now keeps the turn's tool array and sets
`ToolChoice::None`. Every adapter in `tidebreak-router` expresses that mode
natively and none silently downgrades it: Anthropic `{"type":"none"}`, OpenAI
Responses and OpenAI-compatible `"none"`, Gemini `{"mode":"NONE"}`; xAI goes
through the OpenAI-compatible path. The tools+system cache therefore survives on
every chat *except* one running a vendor web search: there the wrap-up clears
`vendor_web_search`, so the adapter renders the wire tool array without the
provider's server-tool entry, rewriting byte zero and forfeiting the cache for
that call. That is deliberate — suppressing a provider-executed server tool with
`tool_choice: none` is a behavior we cannot verify offline, and we will not rely
on it to keep a turn terminal. Everywhere else the messages cache is lost either
way, so this is a strict improvement.

The control is sent only when the request actually carries tools. A chat-only
model, or any turn whose tool surface came out empty, would otherwise pair
`tool_choice` with no tool array — a combination providers reject, hard-failing
the one step that exists to guarantee an answer. With no tools the step is
already terminal by construction.

`tool_choice: none` is a request to the provider, not a guarantee, and this
codebase already assumes the class of OpenAI-compatible runtime that accepts a
control and ignores it. So the wrap-up self-heals: if its calls were all
declined and no prose arrived, it is retried once with `tools: Vec::new()` and
no `tool_choice` — terminal by construction rather than by cooperation. Without
that retry the step produces an empty final response, the turn fails, and the
worker's retry re-enters the same wrap-up and burns attempts against a provider
that has already shown what it does. The retry forfeits the cache, which is the
right trade on a path that is pathological by definition. The belt-and-braces
handling that answers and drops any tool call the wrap-up emits is unchanged;
the retry sits behind it.

## Alternatives Considered

**Keep the utility-model one-shot (the previous design).** Cheap per token, but
it pays full price on every token, and the token count is a whole large
transcript. On an Opus-tier chat a warm-cache read on the conversation's own
model is cheaper than Haiku at full price, and produces a better summary because
a stronger model writes it. Rejected on both cost and quality.

**Keep the utility model but reuse the fitted transcript.** Caches are scoped
per provider and model, so a request on a different model reads nothing however
identical its bytes are. This buys the fidelity loss described above with none
of the saving.

**Enforce the payload with `response_format`.** It is the stronger guarantee and
it is what the code did. On the Messages API it is compiled into an appended
tool and a forced `tool_choice`, which discards both the tools cache and the
messages cache — the two things this change exists to keep. Validation on the
way in already rejects a malformed payload and the producer fails open, so the
wire-level constraint was belt over braces. Rejected.

**Ask for a cheaper read.** There is no read-only or write-suppressed cache
mode; a request either matches a cached prefix or it does not. Nothing to build
against.

**Give the on-demand `/compact` route the turn's frozen tool surface.** It would
raise that route's cache-hit odds, but the surface is assembled in the turn
worker from exec folders, skills, plugins, and network policy, and lifting it
into a plain route is a large change for a path that is often past the five
minute TTL anyway. Left as follow-up work.

## Consequences

- **Warm cache (the common case, mid-turn):** the whole prefix is a cache read
  at ~0.1× the chat model's input price, against ~1.0× of the utility model's
  before. For an Opus-tier chat that is cheaper in absolute terms and the
  summary is written by the better model.
- **Cold cache:** compaction pays full chat-model price for the prefix. This
  happens on the first step after a gap longer than the five-minute TTL, after a
  model or tool-set change, and routinely for `POST /chats/{id}/compact`, which
  runs between turns with an empty tool registry and so rarely matches anything
  a turn sent. On an expensive chat model a cold on-demand compaction is
  materially dearer than the old Haiku call. Accepted: it is bounded (one call,
  once, per compaction), the in-turn path is the common one, and the route's
  cost is visible to the person who asked for it.
- **Compaction is now billed on the conversation's model**, so it appears in the
  chat's own provider spend rather than the maintenance model's. Usage is still
  recorded on the checkpoint and not folded into the turn's totals, and the
  `CompactionStarted`/`CompactionFinished` pair is unchanged.
- **The maintenance call's capability surface grew from nothing to the
  conversation's.** The old utility-model call advertised zero tools. This one
  carries the chat's full tool array and, on a vendor-search turn, a live
  provider web-search budget. Host tool calls are declined without dispatch —
  the stream handler treats any `ToolCallStarted` as a failed checkpoint and
  returns nothing — but a *provider-executed* search is different in kind: it
  runs server-side before the host ever sees the event, so it is billed and has
  already egressed by the time the checkpoint is thrown away. The cost is that
  turn's checkpoint plus one search. The tools ride along anyway because a
  byte-identical prefix is the whole design: an edited tool array is a
  full-price read of the entire transcript, which is the expense this record
  exists to remove, and dropping the tools only for compaction would guarantee
  that outcome on every compaction to avoid an occasional one.
- **An eviction boundary must never slide through already-sent bytes.** The
  standing rule this change also introduces, in `tidebreak-core/src/context.rs`:
  rewriting a message the provider has already cached invalidates the prefix
  from that byte onward, and an agentic step appends messages constantly, so a
  boundary computed as an exact offset re-bills the whole transcript on nearly
  every step. Eviction boundaries therefore advance in quantized jumps —
  floor-rounded where the window is a recency promise that may be overshot
  (`evict_old_tool_result_images`), ceil-rounded where it is a resource cap the
  caller sizes a request against (`evict_images_beyond`). Any future boundary
  over already-sent content is held to the same rule.
- **The prefix identity is now a contract that ordinary changes can break.**
  Anything that makes a step's request differ from the prefix compaction copies
  — a new per-request field, a tool ordering change, a system-prompt suffix
  applied at one call site only — silently reverts the saving with no visible
  failure. Two `ChatRequest` literals exist, the step's and the checkpoint's, so
  nothing structural keeps them aligned; what enforces the parity is the
  whole-struct equality assertion in the test below, which fails the moment a
  new field is set differently on one of them.

Revisit if a provider ships a server-side compaction or summarization primitive,
if the cache TTL or the lookback window changes materially, if the price ratio
between cache reads and a small model's uncached input moves, or if measurement
shows the fidelity loss from summarizing a floor-truncated prefix produces
visibly worse checkpoints.

## Validation

- `malformed_checkpoint_summary_fails_open_to_deterministic_reduction` asserts
  the contract directly: a declined compaction leaves the step's request
  unchanged, so the checkpoint request is compared to the step that follows it
  as a whole struct — the step's request plus the appended instruction plus
  the checkpoint's own `max_tokens`, and nothing else. Whole-struct rather
  than a field list because the hazard this record names is a *new*
  `ChatRequest` field, which a hand-written list cannot see arrive. A
  plausible wrong implementation — one that rebuilds the transcript for the
  checkpoint call, adds a maintenance system prompt, or sets
  `PromptCacheMode::OneShot` — still produces valid checkpoints and would pass
  every other test in the suite.
- The same test still asserts fail-open: a summary the host cannot parse writes
  no checkpoint, closes its status event with `compacted: false`, and leaves the
  turn to complete on deterministic reduction.
- `a_checkpoint_answered_with_a_tool_call_runs_nothing_and_fails_open` covers
  the capability surface above: the call advertises the conversation's tools and
  no `tool_choice`, so the test has a provider take one. Nothing dispatches, no
  checkpoint is written, the attempt fence still holds the turn to one
  maintenance call, and the foreground turn answers.
- `the_checkpoint_output_cap_leaves_room_for_the_chat_s_reasoning` pins the
  output cap against the payload's byte ceiling. The call inherits the chat's
  reasoning and thinking bills against `max_tokens`, so a cap sized for the
  payload alone truncates the JSON at `MaxTokens` and the chat silently stops
  compacting. It also pins the other direction: a chat configured with a lower
  `max_tokens` sends that, not the checkpoint's larger ceiling.
- `creates_projects_and_deduplicates_a_structured_semantic_checkpoint` asserts
  the call carries neither `response_format` nor `tool_choice`, and runs on the
  conversation's model.
- `a_turn_at_the_step_ceiling_concludes_with_an_answer` asserts the wrap-up
  advertises the same tool array as the step before it and sets
  `ToolChoice::None`;
  `a_wrap_up_a_provider_answers_with_a_tool_call_retries_without_tools` asserts
  the self-healing retry: a provider that calls a tool through `none` gets
  one more request with no tools at all, and the turn completes on its prose.
  `a_chat_only_wrap_up_sends_no_tool_choice_and_still_answers` covers the
  tool-less case: a model with no tool surface reaches the wrap-up and the
  request carries neither tools nor `tool_choice`.
- `compaction_needs_no_separately_configured_maintenance_model` asserts the
  on-demand route answers normally on an install with no utility model, where it
  previously returned 422.
- Two router tests pin the wire behavior this design reads from:
  `breakpoints_are_spaced_on_the_layout_that_includes_replayed_reasoning`
  (breakpoints are placed against the rendered block layout, so the previous
  step always leaves an entry inside the 20-block lookback a ride-along needs)
  and `a_one_shot_request_writes_no_cache_entries` (a `OneShot` request emits no
  `cache_control` at all).
- One more entry in the accepted-cold bucket: `POST /chats/{id}/compact` builds
  its `Agent` without a blob store, so image attachments render as text
  stand-ins rather than pixels. That is another way the route's prefix differs
  from a turn's, on top of the empty tool registry — the route was already
  expected to miss the cache, and this does not change that.
