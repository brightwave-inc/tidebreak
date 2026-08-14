# 3. Curated Model Visibility: Recommended Flag and Per-Model Overrides

- Status: Accepted
- Date: 2026-08-10
- Owners: core, desktop
- Related: [`crates/tidebreak-server/src/model_registry.rs`](../../crates/tidebreak-server/src/model_registry.rs)
  (the static catalog and its honesty invariants),
  [`crates/tidebreak-server/src/providers.rs`](../../crates/tidebreak-server/src/providers.rs)
  (`ProviderKind` and credential-backed provider status)
- Supersedes: none

## Context

The model picker lists every catalog model of every credentialed provider.
The catalog has grown to the point where the picker shows long tails nobody
selects deliberately — four generations of Claude Opus, three GPT trim levels,
two Gemini Flash variants. Meanwhile providers *without* credentials are
invisible: the picker gives no hint that the catalog is larger than what a
fresh install shows, and the only path to enabling a provider starts from
settings, unprompted.

**What is true today.** The registry is a static, curated catalog
(`ModelSpec` rows) with tested honesty invariants, including a guard that the
application default model is curated and current. Visibility is binary:
a provider's credentials exist, so all of its models appear; or they don't,
so none do. No per-model preference exists anywhere, and nothing in the wire
contract distinguishes "supported" from "worth showing by default".

**The forces.** The picker is the product's storefront — a long undifferentiated
list buries the models we actually recommend. But the catalog must stay honest:
hiding a model must never mean rejecting it, because chats persist their model
selection and replay must keep working. And whatever carries the preference
becomes a persisted format we live with.

## Decision

Three parts, one vocabulary.

**1. The registry owns the recommendation.** `ModelSpec` gains a
`recommended: bool`. The recommended set is the default-visible set in every
model picker. Curation policy: the current flagship tier of each provider is
recommended; superseded generations, speed/cost trims, and preview variants are
not. The application default model must be recommended — this extends the
existing curated-and-current guard.

**2. User choice is an override map, not a copy of the catalog.** Desktop
settings persist only per-model *deviations* from the recommended flag, keyed
by model id: `{model_id → show | hide}`. Effective visibility is the
recommended flag XOR an override's presence. A catalog refresh therefore gives
new models their flagged default automatically, and user choices survive
catalog updates without a sync step. "Reset to recommended" deletes the
provider's overrides; it does not write "default" values.

**3. Hidden is a picker concern, never a capability.** A hidden model remains
fully valid for existing chats, replay, deep links, and programmatic
selection. A picker whose chat currently uses a hidden model shows that model
as an extra row. Nothing ever silently reroutes a chat off a hidden model —
consistent with the existing rule that unsupported situations produce errors,
not substitutions.

Alongside visibility, the picker surfaces unconfigured providers as collapsed
one-row entries ("Not connected") with a setup CTA deep-linking to that
provider's settings card, and a footer ("N models hidden · Manage models")
keeps the full catalog one click away. These are UI commitments, not wire
contracts, but they are what makes default-hiding acceptable: every hidden
thing remains discoverable.

Deliberately excluded: per-chat or per-project visibility (the override map is
app-global); server-side or remotely-updated curation; hiding *providers* with
credentials (only models are hidden); any notion of ordering/pinning beyond
the existing provider grouping.

## Alternatives Considered

**Do nothing.** Keeps the picker honest but increasingly unusable as the
catalog grows, and leaves unconfigured providers undiscoverable. Rejected: the
catalog is growing on purpose; the picker should not pay for that.

**Trim the catalog instead.** Delete non-flagship models from the registry.
Rejected: the registry's job is honesty about what works, and chats already
reference these models. Deleting a model is a capability statement; this is a
presentation preference.

**Persist the full visibility map (every model, checked or not).** Simpler
reads, but a catalog refresh must then reconcile new models into every user's
map, and a stale map silently hides models we newly recommend. The override
map makes "we changed the default" and "you chose" distinguishable forever.

**A `tier`/`curated` enum instead of a bool.** More expressive (e.g.
flagship/standard/legacy), but no current consumer needs more than visible-by-
default or not, and an enum invites ranking semantics the picker deliberately
does not have. Revisit if a real second consumer appears.

**Per-provider "show all" toggle instead of per-model checkboxes.** Cheaper
UI, but the actual demand is model-shaped ("I want Haiku but not Opus 4.x"),
and the settings card already lists models for other reasons.

## Consequences

- The override map is a persisted format under the pre-v1 mutability rules of
  record 2 — shape changes ride the schema epoch like everything else.
- `recommended` becomes part of the wire contract for the model list; the
  generated TS types regenerate with it.
- Every future catalog addition must take a curation stance — the flag is
  mandatory, so "forgot to decide" is unrepresentable.
- The picker and any future model-choosing surface must consult effective
  visibility rather than the raw catalog, and must handle the
  current-model-is-hidden case. That is a small standing tax on new surfaces.
- Revisit if: curation needs to vary by deployment or team (the static bool
  stops being enough); a second consumer wants graded tiers; or per-project
  model policy arrives, at which point the app-global override map may need a
  scope column rather than a redesign.

## Validation

- Registry test: the application default model is `recommended` (extends the
  existing curated-and-current guard).
- Registry test: every provider with any catalog model has at least one
  recommended model — a provider whose models are all hidden by default would
  render as connected-but-empty, which is indistinguishable from broken.
- A wrong implementation that filters at the *registry* rather than the picker
  would still pass a naive "hidden model absent from picker" test; the test
  that catches it is: a chat pinned to a hidden model still resolves and runs
  a turn, and its picker payload includes that model.
- Override semantics: hiding a recommended model and later "Reset to
  recommended" leaves zero override rows for that provider, not explicit
  "show" rows.
