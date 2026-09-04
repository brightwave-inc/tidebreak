# 88. A Slack session runs where the deployment can run it

- Status: Accepted
- Date: 2026-09-04
- Owners: thet
- Related: [0039](0039-allow-is-a-first-class-code-permission-mode.md) (Allow is a permission mode, confinement is the boundary); [0061](0061-schema-changes-are-migrations.md); [0064](0064-idle-engine-children-are-parked.md) (the machine already runs harness children lazily); [0071](0071-hosted-engines-ride-the-callers-inference.md); [0086](0086-session-access-is-separate-from-ownership.md); `docs/slack-sessions.md`; tidebreak #3178 (track C), #3184, #3216; brightwave-inc/model-gateway#1865
- Supersedes: the "do not ship Slack on the machine engine" paragraph of `docs/slack-sessions.md`

## Context

`docs/slack-sessions.md` said a Slack session runs in a gateway sandbox and
nowhere else, and `CodeRuntime::external_get_or_create` enforced it: a
machine with no sandbox runtime endpoint refused every external session
with `remote_disabled`. The reasoning was sound for the posture it named. A
Slack session was unattended `Allow` with a forge token, and the sandbox
was the defense in depth for exactly that.

Two things changed. First, the machine engine is what every other client
already uses: the desktop, the mobile app, and `agent-mcp` all drive
harness children on the machine, parked when idle (decision 64), with
inference through the relay (decision 71). Second, the rule left whole
deployments without a single Slack session: every standalone machine, and
every gateway deployment until an operator configures a runtime. The
adapter's end-to-end lane (brightwave-inc/model-gateway#1865) found this
on its first run, and the reference deployment found the sandbox path
itself unfinished (#3216).

## Decision

**Execution location is chosen per session from what the deployment has.**
At external get-or-create the machine picks a gateway sandbox when a
sandbox runtime is configured, else its own engine. The choice is stored
on the session as `execution_location` (`sandbox` or `machine`, an appended
migration that backfills existing rows from their workspaces), reported on
the snapshot and the external event stream, and never changes for the
session's life.

**The machine engine is the floor, not an interim path.** A machine
session is an ordinary session: a worktree under the owner's worktree
root, the same worker, the same lazy spawn and park, the same checkpoints
and delivery. The external routes that took a sandbox for granted now
dispatch by location: a message on a machine session lands in the same
durable queue, wakes the worker, and answers `new_turn` when the head is
promoted; interrupt, reap, and the event stream already branch on the
workspace and keep doing so.

**Permission mode follows the location.** A sandbox session keeps `Allow`
because confinement is its boundary (decision 39). A machine session takes
the deployment's default mode; the owner answers approvals from the
desktop or the web, where the session is visible like any other, until
the channel can carry them. `Allow` on the machine is an operator's
setting per deployment, off by default, and a later slice adds it
together with the per-session override the adapter exposes.

## Consequences

- Every deployment can run a Slack session. The reference deployment moves
  its Slack sessions onto the machine engine until the sandbox path is
  finished (#3216); a configured runtime stays the way to choose the
  sandbox.
- The adapter renders which location a session runs on from the snapshot,
  so the thread's first status line can say where the work happens.
- A machine session shares the machine's worktree root and providers with
  the owner's other sessions. The owner-text-only rule and the session
  access rows (decision 86) are the controls; nothing else changes.
- What this does not decide: the operator's `Allow` setting, the
  per-session mode override on the external routes, spend accounting per
  location, and the sandbox path's own defects. Each is its own slice.
