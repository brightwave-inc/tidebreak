# 33. Code Mode Approvals Are First-Class and Deny Can Steer

- Status: Proposed
- Date: 2026-08-15
- Owners: code mode, approvals
- Related: [`0031-harness-adapter-boundary.md`](0031-harness-adapter-boundary.md),
  [`0018-tool-call-narration.md`](0018-tool-call-narration.md),
  [`docs/code-mode.md`](../code-mode.md)

## Context

Every harness code mode drives ships its own permission system: Claude Code
has permission modes and a pluggable permission-prompt channel; Codex CLI has
approval policies and sandbox levels with approval requests in its server
protocol; opencode's server exposes a permission API. Each also ships a bypass
flag that turns the whole system off.

The temptation in this product category is documented by prior art: wrapping
tools launch the harness with its bypass flag because surfacing approvals
through a wrapper is hard, and the wrapper's own approval UI never gets built.
The result is that "supervised coding agent" ships as an unsupervised one.

Tidebreak's chat product already has a considered approval culture: parked
approval cards, grant ladders, and the rule from
[`0018`](0018-tool-call-narration.md) that a tool call cannot argue
for its own approval. Code mode must meet that bar, not undercut it — while
recognizing that code approvals are *foreign*: the harness, not Tidebreak,
defines what needs approval and enforces the outcome.

## Decision

**No bypass by default, ever.** Tidebreak never launches a harness with its
permission-bypass flag as a composed default. This is an enforced invariant:
a denylist test over every adapter's argument construction (including
user-supplied extra arguments from settings) fails the build if a bypass flag
appears in a default launch plan. A user who wants bypass behavior can get it
only by an explicit per-session choice that is rendered as prominently as the
approvals it removes.

**Each adapter wires the harness's native structured approval channel** into
one normalized surface:

- *Claude Code*: a Tidebreak-served permission-prompt tool over the
  loopback MCP endpoint, passed via the harness's permission-prompt-tool and
  MCP configuration flags with a session-scoped token. The harness's tool
  call parks until the user decides; a deny returns the user's message as the
  denial reason, which the model sees.
- *Codex CLI*: the approval request/response methods of its server protocol
  (or the equivalent in exec mode, per what the fixture spike verifies).
- *opencode*: its server permission API.
- *Grok CLI*: whatever structured channel its detected version offers. Absent
  one, the adapter declares `structured_approvals: Unsupported`, and sessions
  on that harness are restricted to permission modes that need no runtime
  approvals — stated visibly at session creation, never silently escalated to
  bypass.

**The normalized shape.** An approval is persisted as
`CodeApproval { kind, session, turn, state, feedback }` where `kind`
classifies what is being asked — command execution (with command and cwd),
file writes (with paths), network access, or other (with the harness's
summary and the raw payload, size-capped). Classification is best-effort and
display-oriented; the *harness's* payload is authoritative for what actually
gets allowed, and the card renders the harness's request rather than a
Tidebreak paraphrase — the [`0018`](0018-tool-call-narration.md)
principle applied to foreign tools.

**Deny can steer.** A decision is `Approve` or `Deny { feedback }`. Denial
feedback travels back through the harness's channel as the denial reason, so
"no — use the fixtures directory instead" redirects the agent in one step
instead of stalling the turn. An undecided approval parks the session in the
`NeedsYou` attention state; approvals survive restarts and are answerable
after reconnect.

**Permission modes reuse the chat vocabulary** (per
[`0030`](0030-code-mode-separate-surface.md)): `Plan` maps to the harness's
read-only or plan mode with mutations refused; `Ask` (the default) maps to
approval-required postures where every request parks on a card; `Auto` maps
to the harness's workspace-write posture where routine edits proceed and the
harness's own policy still escalates sensitive actions to approval. Each
adapter documents its exact flag mapping per mode. A mode a harness cannot
honor is refused at session creation with the reason — never approximated.

Deliberately excluded: standing grants and grant ladders for code approvals
("always allow `cargo test`") — deferred until real usage shows which grants
users actually repeat; Tidebreak-side sandboxing of harness processes — the
harness's own permission system is the enforcement layer in v1.

## Alternatives Considered

**Bypass by default, add Tidebreak-side sandboxing later.** Rejected: it is
the documented failure of this category, it discards the harnesses' own
well-tested permission machinery, and "later" sandboxing of an arbitrary
agent process is a research project, not a backlog item.

**Reuse the chat approval tables and events.** Rejected: chat approvals are
bound to the internal tool registry, call ids, judge machinery, and grant
rungs, none of which exist for foreign tools. The *visual* language is
shared; the data model is not (per
[`0030`](0030-code-mode-separate-surface.md)), though the wire shape stays
inbox-compatible for later convergence.

**Approval by steering text** (answer "please don't" into the conversation).
Rejected: not a channel, not enforceable, and unanswerable when the harness
is mid-tool.

**Tidebreak-authored allow/deny policy engine over harness requests**
(auto-approving classes of commands). Rejected for v1: it reintroduces the
judge problem [`0018`](0018-tool-call-narration.md) guards against,
on foreign payloads we classify only best-effort. Revisit with standing
grants.

## Consequences

The approval channel becomes part of each adapter's contract and its fixture
suite: a harness release that changes its approval protocol breaks a fixture,
not the user's trust.

Sessions in `Ask` mode are only as responsive as the user; the attention
system and inbox-shaped surfacing exist to make that workable. `Auto` mode's
meaning varies by harness and the per-adapter mapping table is user-visible
documentation.

Restricting no-structured-approvals harnesses to non-approval modes narrows
what Grok-tier harnesses can do in v1. That is the honest trade; the
alternative was silent bypass.

Revisit this decision when standing grants are designed, or if a harness
ships an approval channel rich enough to carry Tidebreak's grant ladder
natively.

## Validation

- Per-adapter fixture pairs capturing a real approval request and both
  decision outcomes, replayed in tests.
- An integration test against the scripted harness proving deny feedback
  reaches the model-visible transcript of the following step.
- The bypass denylist test over composed launch plans, including plans with
  user-supplied extra args.
- A restart test: an undecided approval survives a server restart and is
  decidable after reconnect, and the session shows `NeedsYou` throughout.
- A mode-refusal test: creating an `Ask` session on an adapter with
  `structured_approvals: Unsupported` fails with the stated reason.
- A plausible wrong implementation renders approval cards but resolves them
  by writing to the database without completing the harness's channel; the
  scripted-harness test above (the harness observes the decision) must fail
  it.
