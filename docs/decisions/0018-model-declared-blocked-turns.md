# 18. Model-Declared Blocked Turns Are Refused, Not Completed

- Status: Proposed
- Date: 2026-08-14
- Owners: agent runtime, turn lifecycle, CLI
- Related: [`docs-site/content/docs/headless.mdx`](../../docs-site/content/docs/headless.mdx),
  [`docs/tools.md`](../tools.md)
- Supersedes: none

## Context

A model can discover that a required outcome is impossible: a source is absent,
an execution boundary denies the only path, or a required external dependency
is unavailable. Today its only universal terminal action is ordinary assistant
text. If it explains “I cannot complete this as requested” and ends naturally,
Tidebreak records `turn_completed` and a headless CLI exits zero. Automation
cannot distinguish a delivered result from a blocked explanation.

Provider safety refusals already have a refused lifecycle, but the model cannot
portably manufacture a provider stop reason and operational blockage is not a
provider safety policy. Parsing prose for phrases such as “cannot complete” is
language-dependent and would misclassify ordinary explanatory answers.

## Decision

**Foreground agents receive a call-alone `report_blocked` control tool that
terminalizes the turn through the existing refused lifecycle with the bounded
category `blocked`.**

1. The tool accepts a short machine-readable reason code and concise user-facing
   explanation. It is for a required outcome that cannot be delivered after
   available recovery paths were attempted or shown unavailable.
2. The call must be alone in its model step. Tidebreak validates and persists
   the call, then completes the turn as refused; it does not ask the model for a
   second prose-only step that could accidentally become success.
3. The explanation becomes the assistant output. The terminal event is
   `turn_refused` with refusal category `blocked`, and headless CLI behavior
   remains the existing nonzero refused exit.
4. The operating prompt requires the tool when a mandatory artifact, execution,
   successful retry, or requested answer is impossible. It forbids using the
   tool for minor assumptions, partial-but-useful answers, or work the model
   simply prefers not to do.
5. Provider safety refusals remain provider-authored `turn_refused` outcomes;
   the category distinguishes them from model-declared operational blockage.

## Alternatives Considered

- **Infer blockage from assistant prose.** Rejected as brittle, language- and
  provider-dependent, and prone to false positives.
- **Treat every tool failure as a failed turn.** Rejected because agents often
  recover successfully and a failed tool call is not the same as an impossible
  user outcome.
- **Add a new durable `blocked` turn status and event.** Semantically pure, but
  it expands every storage, wire, renderer, and driver state machine when the
  existing refused lifecycle already supplies the required non-success
  semantics and explanatory output.
- **Reuse `ask_user_questions`.** Rejected because some blockers have no user
  decision that can resolve them, and undriven questions mean “waiting for
  input,” not “finished unsuccessfully.”
- **Do nothing.** Leaves successful automation indistinguishable from an agent
  reporting that it did not do the work.

## Consequences

The model-facing tool vocabulary gains one terminal control action and refusal
metrics will include both provider policy refusals and operational blockage,
distinguished by category. A malicious or confused model can still declare a
solvable task blocked, so prompt guidance and evals must make misuse visible;
the tool improves outcome honesty, not model capability.

Revisit if refused analytics need a first-class blocked status, or if workflows
require resumable blockers rather than terminal handoff.

## Validation

- A required missing-source task that exhausts available recovery calls uses
  `report_blocked`, persists the explanation, emits `turn_refused` category
  `blocked`, and exits nonzero through the CLI.
- A recoverable first tool failure followed by success completes normally and
  never invokes `report_blocked`.
- A blocked call emitted with sibling calls is refused with corrective
  call-alone guidance rather than terminalizing ambiguously.
- Invalid/overlong reason codes or explanations are rejected before terminal
  state changes.
- Provider-authored safety refusal behavior remains unchanged.
