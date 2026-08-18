# 39. Allow Is a First-Class Code Permission Mode

- Status: Proposed (amended 2026-08-18, see [Amendment](#amendment-2026-08-18))
- Date: 2026-08-17
- Owners: code mode, approvals
- Related: [`0033-code-mode-approvals.md`](0033-code-mode-approvals.md),
  [`0038-auto-is-a-declared-capability.md`](0038-auto-is-a-declared-capability.md)

## Context

Chat already has four permission modes — `Plan`, `Ask`, `Auto`, `Allow` —
in ascending autonomy. Code mode reused the first three and left `Allow`
out: [`0033`](0033-code-mode-approvals.md) reserved bypass for "an explicit
per-session choice that is rendered as prominently as the approvals it
removes", and [`0038`](0038-auto-is-a-declared-capability.md) kept the
bypass-flag denylist closed so unsupervised Auto would never be a flag we
add.

That split is now the product gap. Users who want the engine's own
allow-everything posture have no first-class way to choose it. Each
harness already has one:

- Claude Code: `--dangerously-skip-permissions` (with
  `--allow-dangerously-skip-permissions` so print mode will honor it)
- Codex CLI: `thread/start` `sandbox=danger-full-access` +
  `approvalPolicy=never`
- opencode: the `build` agent with every permission rule `allow`
- Grok CLI: `--always-approve` (accepted on the captured 1.0.4)

0033 already forbids composing those as a default. It does not forbid
mapping them onto a named mode the user picks.

## Decision

**`CodePermissionMode` gains `Allow`.** The token is `allow`, matching
chat. The scale is `Plan < Ask < Auto < Allow`. Default stays `Ask`.

**Each adapter maps every mode it can honor onto that engine's native
setting.** The mapping is exhaustive and per-mode: a mode the engine cannot
honor is still refused, never approximated. Allow is not "Auto plus a
bypass flag we hope works"; it is the engine's documented allow-everything
posture, composed only when the session is in Allow.

**`Allow` is gated by its own capability flag.** `HarnessCaps` gains
`allow_mode: CapLevel`. Session creation refuses Allow on that flag, the
same way Plan / Ask / Auto each stand on their own. No mode is derived
from another mode's flag.

**The bypass denylist stays closed for every other mode.** Plan, Ask, and
Auto launch plans — including user-supplied extra arguments — still fail
the build if a known bypass flag appears. Allow is the one mode that may
compose the engine's bypass. That is the explicit per-session choice 0033
reserved; it is not a default, and it is stated where the mode is chosen.

**Existing Auto mappings do not change.** Claude Auto is still
`acceptEdits`. Codex Auto is still `workspace-write` + `on-request`.
opencode Auto still allows edits and asks for bash. Grok Auto is still the
default headless posture with no permission flags. Grok Allow is the
distinct `--always-approve` flag; Auto and Allow are not collapsed.

Deliberately excluded: changing a live session into Allow (still deferred
with every other mid-session mode change); standing grants; Tidebreak-side
sandboxing of an Allow session.

## Alternatives Considered

**Keep Allow out of code mode (do nothing).** Honest to 0038's closed
denylist, but it leaves the chat vocabulary incomplete on the surface that
actually drives foreign engines, and it forces anyone who wants bypass to
smuggle it through extra argv — which the denylist then rejects.

**Treat Grok's unsupervised Auto as Allow and hide Auto.** Rejected: Auto
is the engine's default headless posture; Allow is an explicit bypass flag
the CLI accepts. Collapsing them would approximate two settings as one.

**Expose each engine's native mode names in the picker.** Rejected: the
product vocabulary is Tidebreak's, and a future chat–code merge depends on
the tokens staying shared. Native names belong in adapter mapping tables
and doctor detail, not on the create control.

## Consequences

`HarnessCaps` changes shape again, which ripples through the wire
snapshot, the generated renderer types, the CLI doctor summary, and every
caps literal. The `code_session.permission_mode` check constraint grows
`allow`, so the desktop schema epoch bumps in the same change.

Allow sessions do not wire the approval channel. Surfaces that offer the
mode must state that the engine's permission system is off.

Revisit if a harness ships an allow-everything posture that is not a
bypass flag (a first-class native mode with its own confinement story), or
if usage shows Allow being chosen without the statement landing.

## Validation

- Per-adapter mapping tests: Allow composes the documented native setting;
  Plan / Ask / Auto still do not contain a bypass flag.
- A server mode-refusal test: Allow on an adapter with
  `allow_mode: Unsupported` fails with the stated reason even when
  `auto_mode: Supported` — the case a wrong implementation (deriving Allow
  from Auto) would still pass.
- UI: the mode list is computed from the four flags, and the Allow
  statement renders exactly when Allow is the selected create mode.
- The denylist test still fails a Plan / Ask / Auto plan that includes a
  bypass flag, including via extra argv.

## Amendment (2026-08-18)

**The create default is now the most autonomous mode the engine honors**,
walking `Allow → Auto → Ask → Plan` and stopping at the first flag that
says `supported`. The rest of this record stands: the scale, the per-mode
capability flags, the exhaustive adapter mapping, and the closed bypass
denylist for every mode but Allow are unchanged.

Why: supervision cost dominated the create flow. Every new session opened
in Ask and then spent its first minutes on approvals for reads and edits
inside a throwaway worktree — the confinement code mode already provides
([`0032`](0032-code-workspaces-worktrees-checkpoints.md)). Choosing the
posture per session stays available; it is no longer the price of starting.

What does not change: the statement. Allow and unsupervised Auto still say
what they are next to the control that selects them, and now that they can
arrive by default, that statement renders on open rather than only after a
deliberate pick.

Revisit if an unsupervised default causes an incident — a session that
damages something outside its worktree, or acts on a prompt the reader
would have stopped at the approval — or if readers report not seeing the
posture they were started in.
