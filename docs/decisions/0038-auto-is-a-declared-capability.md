# 38. Auto Is a Declared Capability, and Unsupervised Auto Is Stated

- Status: Accepted
- Date: 2026-08-17
- Owners: code mode, approvals
- Related: [`0031-harness-adapter-boundary.md`](0031-harness-adapter-boundary.md),
  [`0033-code-mode-approvals.md`](0033-code-mode-approvals.md),
  [`crates/tidebreak-harness/fixtures/grok/1.0.4/manifest.toml`](../../crates/tidebreak-harness/fixtures/grok/1.0.4/manifest.toml)

## Context

[`0033`](0033-code-mode-approvals.md) maps Tidebreak's three permission modes
onto each harness and rules that a mode a harness cannot honor is refused at
session creation — never approximated, never silently escalated to bypass.
For harnesses without a structured approval channel it says sessions "are
restricted to permission modes that need no runtime approvals".

The implementation came out stricter than that sentence: `Ask` and `Auto`
both required `structured_approvals: Supported`, and `Plan` required
`plan_mode: Supported`. For Grok CLI 1.0.4 the capture found no approval
channel *and* a plan flag that does not confine (`--permission-mode plan`
and `--sandbox read-only` both wrote files), so every mode was refused and
the engine was listed but unusable.

What the strict reading missed is that `Auto` needs no runtime approvals to
be honored. Re-probed on the same build (1.0.4 d846eb93, 2026-08-17): the
default headless posture — `--prompt-file` + `--output-format
streaming-json`, **no permission flags composed** — executes write tools
without prompting. That *is* a workspace-write auto posture. What it lacks
is the escalation half of 0033's Auto gloss ("the harness's own policy still
escalates sensitive actions"), because there is nothing to escalate to.

So the real question is not whether Grok can run — it can, today, with no
bypass flag — but how the product distinguishes *supervised* Auto (routine
edits proceed, sensitive actions park on approval cards) from *unsupervised*
Auto (everything proceeds), without hardcoding harness names into policy.

## Decision

**`Auto` is gated by its own capability flag.** `HarnessCaps` gains
`auto_mode: CapLevel`: whether the adapter can select a workspace-write /
auto posture for this engine version. Session creation refuses each mode on
its own flag — `Plan` on `plan_mode`, `Ask` on `structured_approvals`,
`Auto` on `auto_mode`. No mode is ever derived from another mode's flag.

**Whether Auto is supervised is read from `structured_approvals`.** The two
flags together describe the posture honestly: `auto_mode: Supported` +
`structured_approvals: Supported` is 0033's supervised Auto;
`auto_mode: Supported` + `structured_approvals: Unsupported` is unsupervised
Auto — every action the engine takes proceeds, nothing parks. Surfaces that
offer the mode (the create dialog, the start-session prompt, the CLI) must
state the unsupervised variant visibly at selection time. That satisfies
0033's "stated visibly at session creation, never silently escalated":
the statement is the product's, made where the choice is made.

**Grok CLI 1.0.4 declares `auto_mode: Supported` and maps Auto to its
default headless posture**, composing no permission flags at all. `Plan` and
`Ask` stay refused on this version: the plan flag does not confine, and
there is still no approval channel. The bypass denylist is untouched —
`--always-approve`, `--yolo`, and `bypassPermissions` remain forbidden in
composed plans; unsupervised Auto is the engine's own default, not a flag we
add.

**Existing adapters keep their behavior.** Claude Code, Codex CLI, and
opencode declare `auto_mode` exactly where their Auto worked before — for
Claude that means the same version gate as its approval channel, since
unsupervised Auto has not been probed there and `Unknown` is the honest
answer on unverified versions.

Deliberately excluded: offering a synthetic Plan for Grok out of `--deny`
rules (a posture the engine's own plan flag failed to honor — captured
denials exist, but composing them would be the approximation 0033 forbids);
and driving Grok over ACP (`grok agent stdio`) for structured approvals — no
`session/request_permission` pair has been captured, so it stays
`not_verified` in the manifest.

## Alternatives Considered

**Keep Grok fully refused (do nothing).** Honest but strictly worse than
honest Auto: the engine executes unsupervised by default in every terminal
its user already runs it in; Tidebreak refusing to drive it protects nothing
and forfeits the workspace isolation, checkpoints, and review surface that
are the product's actual safety contribution.

**Derive Auto from `structured_approvals || best_effort_tier`** or hardcode
the grok kind in the server gate. Rejected: policy keyed on harness identity
rots the moment a version changes behavior; capability flags are the
established carrier ([`0031`](0031-harness-adapter-boundary.md)) and the
caps struct has no `Default` precisely so a new flag forces every adapter to
answer.

**Synthesize Plan from `--deny Edit --deny Write --deny Bash`.** The denials
individually worked in capture, but a composed denylist is an enumeration —
every tool not on it still runs. Presenting that as "Plan: mutations
refused" would promise more than the engine enforces.

**Adopt ACP and build a real approval channel for Grok.** The right end
state if Grok's ACP ships a capturable `session/request_permission`
round-trip; not buildable today on fixtures-before-parsers grounds.

## Consequences

`HarnessCaps` changes shape, which ripples through the wire snapshot, the
generated renderer types, the CLI doctor summary, and every caps literal in
tests — a compile-time sweep by design.

Unsupervised Auto puts real weight on the statement at selection time and on
what Tidebreak itself provides around an unsupervised engine: worktree
isolation, per-turn checkpoints, diffs, and review before merge. Those, not
runtime approvals, are the supervision story for Grok-tier engines.

Revisit when a Grok version honors its plan flag (unlock `Plan` from a new
capture), or ships a capturable approval channel (unlock `Ask`, and Auto
becomes supervised), or if usage shows unsupervised Auto being chosen
without understanding — the visible statement is the mitigation, and if it
fails the mode should be demoted behind an explicit setting.

## Validation

- Adapter tests: Grok accepts `Auto` and still refuses `Plan` and `Ask`;
  the composed Auto launch plan contains no permission or bypass flags (the
  existing denylist test extended to the Auto plan).
- A server mode-refusal test: `Auto` on an adapter with
  `auto_mode: Unsupported` fails with the stated reason even when
  `structured_approvals: Supported` — this is the case a plausible wrong
  implementation (deriving Auto from approvals) would still pass without.
- UI: the mode list is computed from the three flags, and the unsupervised
  statement renders exactly when `auto_mode` is supported while
  `structured_approvals` is not.
- The Grok fixture manifest records the 2026-08-17 re-probe: default
  headless executed a write tool unprompted; `--permission-mode plan` wrote
  again.
