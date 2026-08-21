# 57. One Claude Code Child Per Session, Not Per Turn

- Status: Accepted
- Date: 2026-08-21
- Owners: code mode, harness integration
- Related: [`0031-harness-adapter-boundary.md`](0031-harness-adapter-boundary.md),
  [`0033-code-mode-approvals.md`](0033-code-mode-approvals.md),
  [`0046-rich-turn-input-rides-harness-capabilities.md`](0046-rich-turn-input-rides-harness-capabilities.md),
  [`0055-multiple-sessions-per-workspace.md`](0055-multiple-sessions-per-workspace.md)

## Context

[`0031`](0031-harness-adapter-boundary.md) names "NDJSON child per turn vs
long-lived JSON-RPC vs HTTP+SSE" as a structural difference the adapter
boundary absorbs. It does absorb it — every adapter produces the same events —
but the process model is not only a parsing detail. It is paid for on every
turn, in latency and in tokens.

The Claude Code adapter spawned a fresh `claude -p` for each turn and closed
its stdin to deliver the prompt. The Codex adapter holds one
`codex app-server --stdio` child for the session; the opencode adapter holds
one `opencode serve`. Only Claude Code, the reference tier, paid per turn.

**Latency.** On a 1600-file repository, `turn_started` → `session_started` was
2.855 s, 2.165 s, and 2.714 s on successive turns, before any model work. A
turn that answered "what is 2+2" with 3 output tokens took 5.461 s, of which
3.191 s was not model time.

**Tokens.** Respawning re-derives the engine's environment block — cwd, git
status, a directory snapshot — which rewrites the tail of the cached prompt
prefix. An audit correlated this 5 for 5 against worktree changes: a turn that
changed no files was followed by a turn writing 202–848 cache-creation tokens,
while a turn that changed 1–5 files was followed by 11,447 / 12,268 / 19,699.
Priced at the documented cache rates that was $0.378 across 3 of 12 turns,
10.5% of the run's spend. On one turn, 85% of the turn's cost was the
redundant write, for the answer "4".

The machinery to stop paying this was already in the adapter and used for one
case only. `--input-format stream-json` was passed when a turn carried images
([`0046`](0046-rich-turn-input-rides-harness-capabilities.md)), and the encoder
already wrote a stream-json user line. What forced the exit was closing stdin
after that one message.

Before deciding, the following were verified live against `claude` 2.1.238 and
recorded in `crates/tidebreak-harness/fixtures/claude-code/2.1.233/manifest.toml`:

- With `--input-format stream-json` and stdin held open, one child answers turn
  after turn. It exits 0 when stdin closes.
- `system/init` is reprinted for **every** user message, with the session id
  unchanged. It is not printed at all until the first user message arrives, so
  there is no engine-observable "session opened" before a prompt.
- A text-only user message is not echoed back on stdout.
- `control_request {subtype: interrupt}` on stdin aborts the running turn: the
  engine answers `control_response` success, writes a
  `[Request interrupted by user]` user line, and ends the turn with
  `result terminal_reason=aborted_streaming`. The child stays up and runs the
  next turn. An interrupt with no turn running is answered success and changes
  nothing.
- `--resume <id>` together with `--input-format stream-json` keeps the same
  session id, so a replacement child rejoins the session rather than starting
  one.

The same four prompts, run once per-turn-child and once on one child, in
matched scratch repositories:

| turn | send → init (per turn) | send → init (one child) | cache creation (per turn) | cache creation (one child) |
| ---- | ---------------------- | ----------------------- | ------------------------- | -------------------------- |
| 1    | 1.601 s                | 0.687 s                 | 4,455                     | 4,453                      |
| 2 (writes a file) | 0.783 s   | 0.032 s                 | 3,383                     | 3,413                      |
| 3    | 0.722 s                | 0.050 s                 | **7,869**                 | **29**                     |
| 4    | 0.601 s                | 0.017 s                 | 52                        | 14                         |

Turn 3 is the one that matters: it follows a turn that changed the worktree.

## Decision

The Claude Code adapter holds **one child per session** and feeds it one
stream-json user line per turn.

1. `--input-format stream-json` is always passed, not only for image turns.
   Stdin stays open for the life of the session. Every turn is one user line.
2. A turn ends on the stream's own terminal event — the `result` line the
   parser already maps to `TurnCompleted`, `TurnFailed`, or `TurnInterrupted`.
   The process exiting is no longer the end-of-turn signal; it is a failure
   signal.
3. `session_started` fires once per child rather than once per turn. The parser
   is owned by the child and its existing guard absorbs the repeated
   `system/init`.
4. **Interrupt is a control request first.** The first stop for a turn writes
   `control_request {subtype: interrupt}` to stdin; the engine ends the turn
   itself and the session stays warm. A second stop for the same turn — or a
   stdin that will not take the request — stops the process. That is not a
   session-ending event either: the next turn respawns.
5. **A dead child is replaced, not fenced.** Before writing a turn the adapter
   checks whether the child is still running, and a write that fails on a
   reused child is retried once on a replacement. The replacement launches with
   `--resume <session id>`, so the user's message lands on the same transcript.
   `child_pid` and the pid watch behave as before; `spawn_epoch` is untouched.
6. **The first spawn resumes; later turns do not re-resume.** `--resume` is a
   launch flag, so it is applied when a child starts, from whatever ref the
   session holds at that moment.
7. **A model change respawns.** `--model` is also a launch flag. A turn asking
   for a model the current child was not launched with retires that child and
   starts one on the requested model, resuming the session.
8. The child is spawned **lazily, on the first turn**, not at session attach.

Deliberately excluded: mid-turn steering. Holding stdin open makes it
reachable, and `mid_turn_steering` stays `Unknown` for this adapter until it
has been captured and fixtured under [`0031`](0031-harness-adapter-boundary.md).
Nothing here claims it.

## Alternatives Considered

**Do nothing.** Rejected. The cost is per turn, measured, and paid by the
reference-tier harness — the one every code-mode feature is required to work
on. Nothing else in the product spends a tenth of a run's budget on
re-deriving state it already had.

**Spawn the child eagerly at session attach.** This is what Codex and opencode
do, and it would move the remaining first-turn boot cost off the first turn.
Rejected for now on two facts. First, `system/init` is not printed until a user
message arrives, so an eager spawn still could not fire `session_started` at
session open without sending something — the stated benefit is not actually
available. Second, boot re-attach in
`crates/tidebreak-server/src/code/runtime.rs` restores *every* non-ended,
non-fenced session concurrently with no cap; making Claude Code attach eagerly
would launch that many `node` processes at every app launch. Eager spawn is
worth revisiting behind a bound on that loop.

**Keep respawning but strip the environment block.** Rejected: the environment
block is the engine's, not ours, and suppressing it would change what the model
knows about the worktree to save tokens. That is a correctness trade made for a
cost reason.

**Keep the kill-based interrupt.** Rejected. Killing the child was only
acceptable because the child was disposable. With one child per session it
would throw away the warm process the decision exists to keep, and it discards
the engine's own `result` — the turn would end on a signal rather than on
`terminal_reason: aborted_streaming`, which is strictly less information.

**Reuse the child across a model change.** Rejected as unverified. The control
channel may accept a model change, but nothing in the captures shows it, and
[`0031`](0031-harness-adapter-boundary.md) forbids writing to a protocol we
have not observed. Respawning is correct and costs a spawn only when the user
switches models.

## Consequences

A `claude` process now lives for as long as a session that has run a turn,
instead of for the length of one turn. Three things follow.

*The session row carries a live pid while idle.* Crash recovery
(`crates/tidebreak-server/src/code/recovery.rs`) scans only `Running` sessions,
so an `Idle` row with a recorded pid is not probed; the next attach overwrites
it. This is safe rather than merely tolerable, because an orphaned child reads
EOF on the stdin its dead parent held and exits on its own. If recovery is ever
extended to non-`Running` rows, that pid becomes meaningful and must be probed,
not assumed.

*Worker replacement still ends the child.* Reap and permission-mode changes
stop the worker, which calls `shutdown` on the engine session, which stops the
process. The approval token minted per attach therefore cannot outlive its
child, and a replaced worker's child cannot keep prompting against a revoked
token.

*Approvals are unaffected.* They ride the loopback HTTP MCP endpoint
([`0033`](0033-code-mode-approvals.md)): the request parks a oneshot inside the
axum handler and the decision returns as that same HTTP response. Nothing in
that path touches the child's stdin or waits for it to exit. An approval can
only arise inside a turn, so the between-turns window a live child now opens
carries no approval traffic.

Consumers that keyed off `session_started` arriving once per turn will now see
it once per session. Nothing in the CLI or desktop did — the CLI folds it into
a lifecycle branch and the server journals its own at attach — but the change
is observable in the journal.

Revisit this decision if Claude Code stops serving multiple messages on one
stdin, if holding a child for a session's lifetime proves too expensive in
memory across many open sessions, or if the boot re-attach loop grows a bound
that makes eager spawn worth taking.

## Validation

- Fixture replay of a two-turn stream on one child asserting exactly one
  `SessionStarted` across two `TurnCompleted`s, and that the second turn's
  `cache_creation_input_tokens` is a fraction of the first's. A wrong
  implementation that reset the parser per turn would emit two `SessionStarted`
  and pass every other assertion.
- An adapter test running two turns and asserting the same pid answered both,
  that the process is alive between them, and that exactly one user line per
  turn reached the child's stdin.
- An interrupt test asserting the turn ends `Clean` with `TurnInterrupted`
  emitted, the child survives, and the next turn runs on the same pid. A
  kill-based implementation fails the pid assertion.
- A second-stop test asserting the process is taken and the *next* turn still
  succeeds, on a replacement launched with `--resume`.
- A dead-child test asserting the next turn respawns with `--resume <id>` and
  completes, rather than failing or fencing the session.
- The existing failed-child test, unchanged: a child that exits non-zero
  without a `result` still reports its status and stderr as an incomplete turn.
