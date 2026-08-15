# 31. Harness Adapters: One Normalized Event Vocabulary Behind a Per-Harness Boundary

- Status: Accepted
- Date: 2026-08-15
- Owners: code mode, harness integration
- Related: [`0030-code-mode-separate-surface.md`](0030-code-mode-separate-surface.md),
  [`0033-code-mode-approvals.md`](0033-code-mode-approvals.md),
  [`docs/model-providers.md`](../model-providers.md),
  [`docs/code-mode.md`](../code-mode.md)

## Context

Code mode drives external coding-agent CLIs — Claude Code, Codex CLI, Grok
CLI, opencode. Each speaks its own machine-readable protocol: Claude Code
emits a newline-delimited JSON stream in print mode and accepts streamed JSON
input; Codex CLI has a JSONL exec mode and a long-lived JSON-RPC server mode;
opencode exposes an HTTP server with an event stream; Grok CLI's surface is
the youngest and least settled. All of them revise these protocols on their
own release cadences, without notice to us.

Two failure modes are known from prior art in this category and must be
designed out:

1. **Parsers written from assumption.** It is easy to write a parser against
   what a protocol *plausibly* emits — documentation, a changelog, memory —
   and ship code that never matches a real byte stream. Such a parser fails
   silently: the product degrades to whatever its fallback is, and nobody
   notices because nothing errors.
2. **Terminal scraping.** Driving the interactive TUI through a
   pseudo-terminal and inferring state from redrawn screen bytes caps the
   product permanently: no structured conversation, no reliable approval
   detection, no durable history, prompt delivery by timing heuristics.

Meanwhile Tidebreak already has a written philosophy for exactly this shape of
problem: [`docs/model-providers.md`](../model-providers.md) tiers providers,
requires capability flags to say honestly what works where, and forbids
silent degradation. The same philosophy applies one level up, to whole
agents.

## Decision

Every harness is wrapped by an **adapter** in a dedicated crate,
`tidebreak-harness`, that translates the harness's native machine-readable
protocol into one normalized vocabulary, `CodeEvent`, defined in
`tidebreak-core` alongside the domain model. Orchestration, persistence, and
UI consume only `CodeEvent`; nothing above the adapter parses harness-native
bytes. Harnesses are driven exclusively through their non-interactive,
machine-readable modes — never through a pseudo-terminal
([`0036`](0036-code-mode-auxiliary-terminals.md) covers the one PTY in the
product, which is for the user's shells, not the harness).

Three rules are part of the decision, each with an enforcement:

1. **Fixtures before parsers.** A parser for a harness protocol may only be
   written or modified against captured streams from a real CLI invocation,
   checked into `crates/tidebreak-harness/fixtures/<harness>/<version>/`
   together with a manifest recording the exact invocation and any redaction.
   Every parser test replays fixtures and asserts the exact normalized event
   sequence. A parser change with no fixture change is a review defect.
2. **Explicit capabilities.** Each adapter states a `HarnessCaps` value for
   every capability flag — resume, streaming deltas, structured approvals,
   mid-turn steering, plan mode, reasoning levels, native file-change events,
   native interrupt — as `Supported`, `Unsupported`, or `Unknown`. The struct
   is constructed exhaustively (no `Default`), so adding a flag forces every
   adapter to answer. An unsupported capability degrades *visibly* in the
   product (a stated limitation, a disabled control with a reason), never
   silently.
3. **Version-detect, don't assume.** Adapters probe the installed CLI's
   version and select parsing behavior accordingly. An unknown newer version
   runs in best-effort mode with unrecognized-event *counting* — a counter
   surfaced in the session UI and the harness doctor page — never silent
   dropping. Raw unrecognized payloads go to a size-capped per-session debug
   log for later fixture capture; they never enter the journal.

Adapters are tiered like model providers: Claude Code is the reference tier
and every code-mode feature must work there; Codex CLI is second; opencode
third; Grok CLI is best-effort. A feature that cannot be expressed for a
harness gets a per-adapter decision, even when that decision is "not
supported here", recorded in the caps value.

The contract is engine-neutral on purpose. Nothing in the traits, the event
vocabulary, or the capability flags may assume the engine is a *coding*
agent: [`0030`](0030-code-mode-separate-surface.md) names a destination in
which Tidebreak's own internal loop is one more implementation of this
contract, selected where its capabilities are the best fit. A proposed
addition to the contract that only makes sense for code is a smell to
resolve in review, not a convenience to accept.

`CodeEvent` follows the chat journal's conventions exactly — internally
tagged serde enum, `#[non_exhaustive]`, bounded payloads with hint events
pointing at renderer-safe routes for anything large — so the two journals
stay convergent per [`0030`](0030-code-mode-separate-surface.md).

Deliberately excluded: a plugin or out-of-process adapter API. Adapters are
in-tree Rust; a third-party harness lands as a PR, not a plugin, until the
contract has survived several first-party adapters.

## Alternatives Considered

**Drive the interactive TUIs through a PTY and scrape output.** Rejected as a
foundation for the reasons above: it forfeits structure permanently and makes
approvals, resume, and steering heuristic. (A PTY escape hatch for running a
harness interactively is separately deferred, not designed here.)

**Per-harness product surfaces.** Let each harness ship its own view and
semantics. Rejected: N harnesses × M features with no shared approval or
attention surface, and the UI would encode protocol details the adapter
boundary exists to contain.

**Define `CodeEvent` in `tidebreak-harness`.** Rejected: the journal persists
these events, and persistence contracts live where the store lives, in
`tidebreak-core`. The harness crate depends on core, not the reverse.

**One generic "JSON-lines adapter" configured per harness.** Rejected: the
protocols differ structurally (NDJSON child per turn vs long-lived JSON-RPC vs
HTTP+SSE), and a configuration language expressive enough to bridge that is a
worse programming language.

**Trust documentation instead of fixtures.** Rejected: the documented streams
in this category have already diverged from shipped behavior within single
release cycles, and a wrong parser fails silently.

## Consequences

A new crate joins the workspace with an unusual test asset: captured protocol
fixtures that must be re-captured when harness versions move. That is a
maintenance cost accepted deliberately — it converts "the parser silently
rotted" into "a fixture capture is stale", which is visible and mechanical.
Fixture capture requires a machine with the real CLIs installed and
authenticated; CI replays fixtures but cannot capture them.

The adapter boundary makes protocol drift a bounded, per-adapter event. It
also means code mode's feature ceiling per harness is set by that harness's
machine-readable surface; where a harness offers less than its TUI does, code
mode says so rather than faking it.

Revisit this decision if a cross-harness standard protocol emerges and at
least two of our harnesses speak it natively, or if an adapter's protocol
churns so fast that fixtures cannot keep up — the latter would argue for
demoting that harness a tier, not for weakening the rules.

## Validation

- Fixture-replay tests per adapter asserting exact `CodeEvent` sequences,
  including at least: a plain text turn, a tool-using turn, an approval
  request with both outcomes, resume, interrupt, and an error/limit case.
- A repository check that every adapter module has a corresponding fixtures
  directory with a manifest.
- The exhaustive-construction property of `HarnessCaps` (no `Default` impl;
  a unit test constructs it and the compiler enforces completeness).
- A serde round-trip and variant-stability test on `CodeEvent` mirroring the
  chat event enum's test.
- An unrecognized-event test: feeding a stream containing unknown event types
  increments the surfaced counter and drops nothing else on the floor.
- A plausible wrong implementation parses fixtures correctly but quietly
  discards unknown lines; the counting test above must fail it.
