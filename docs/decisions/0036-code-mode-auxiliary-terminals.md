# 36. Auxiliary Terminals Are Ephemeral Byte Streams, Never the Harness Interface

- Status: Proposed
- Date: 2026-08-15
- Owners: code mode
- Related: [`0031-harness-adapter-boundary.md`](0031-harness-adapter-boundary.md),
  [`0035-code-mode-wire-contract.md`](0035-code-mode-wire-contract.md),
  [`docs/code-mode.md`](../code-mode.md)

## Context

Code mode's primary interface is structured
([`0031`](0031-harness-adapter-boundary.md)), but a workspace is a real
checkout and the user verifying an agent's work needs a shell in it: run the
tests, poke at state, try the build. Sending them to a separate terminal
application with a copied path is friction exactly at the moment of review.

A terminal is also the classic scope trap in this category: once a PTY
exists, it is tempting to run the harness in it "as a fallback", and the
structured product quietly becomes a terminal wrapper. This record draws
that line.

Terminal output is unlike journal data: it is unbounded, low-signal per
byte, and valuable only while the shell lives. Persisting it would be
storing noise durably.

## Decision

A workspace may open **auxiliary terminals**: interactive shells (the user's
`$SHELL`) with the worktree as working directory, rendered in the workspace
UI as a secondary surface. They exist for the user's hands, not the
harness's.

**The harness never gets a PTY.** Adapters have no pseudo-terminal API, so
the constraint holds by construction: the only PTY code in the product lives
in the terminal module, and the harness session contract
([`0031`](0031-harness-adapter-boundary.md)) speaks pipes and protocols
only.

**Ephemeral by definition.** Terminal bytes live in a bounded in-memory
ring buffer per terminal. They are not journaled, not persisted, and not
part of any transcript; on server restart, terminals are gone and the UI
shows "shell ended" rather than pretending continuity. Truncation (ring
overflow) is surfaced inline in the stream, not hidden.

**Cursor-pull streaming.** Deliberately unlike the journal
([`0035`](0035-code-mode-wire-contract.md)) and matched to the data's
nature: the server publishes only coalesced activity notices (tens of
milliseconds granularity) on the updates channel; a client with the
terminal open pulls bytes from a monotonic byte cursor over a plain GET,
and writes keystrokes and resizes over plain POSTs. Reconnect and
multi-client reads are trivially correct — a reader is just a cursor — and
the render rate is decoupled from the producer rate. A quiet terminal costs
nothing.

**Bounded everywhere.** Terminals per workspace are capped; ring size is
capped; read responses are capped; input writes are size-checked. The
terminal is a convenience, not a data plane.

Deliberately excluded: terminal persistence or scrollback restore across
restarts; shared/broadcast terminals; and running any harness in a PTY —
that idea goes to [`docs/deferred.md`](../deferred.md) as an explicitly
rejected-for-now escape hatch, to be reconsidered only if a harness's
machine-readable surface proves unusable.

## Alternatives Considered

**No terminal at all** (open the user's own terminal app at the path).
Rejected by product decision: verification belongs next to review, and the
cost of a bounded auxiliary surface is modest. The line drawn here — never
the harness — is what keeps it from growing.

**Journal the terminal bytes.** Rejected: unbounded, low-value durable data,
and a transcript of a human poking at a shell is not part of the session's
reviewable record. Anything worth keeping (a test run the agent should see)
belongs in the conversation, said to the agent.

**Push bytes over a WebSocket per terminal.** Rejected: reconnect and
multi-client semantics need cursors anyway, backpressure on a fast producer
is harder to reason about, and the pull model makes an idle background
terminal free.

**Run the harness interactively in the terminal as a fallback mode.**
Rejected: it reintroduces the terminal-wrapper failure mode — no structured
events, no real approvals, heuristic prompt delivery — through the back
door, and its existence would sap the pressure to fix adapters when
protocols drift.

## Consequences

This introduces the product's first and only pseudo-terminal dependency and
its first terminal-emulator UI dependency, both exact-pinned; the UI cost is
borne by the code-mode bundle. The renderer treats terminals as ephemeral
views: closing and reopening re-fetches recent bytes from the ring rather
than retaining renderer state.

Because nothing persists, support questions of the form "what did the user
run" have no answer by design; that is the privacy-respecting default for a
surface that belongs to the user's hands.

Revisit this decision if a harness's machine-readable surface proves
genuinely unusable (the deferred escape hatch), or if users demonstrably
need shared terminal state across restarts — which would argue for an
opt-in, clearly-labeled record mode, not silent persistence.

## Validation

- Ring-buffer unit tests: wraparound, cursor semantics at overflow, the
  inline truncation marker, read caps.
- A restart test: terminals are absent after restart and the API reports
  them ended; no terminal bytes exist anywhere durable (assert the store
  contains none).
- A coalescing test: a fast producer yields bounded notice frequency while
  a pulling reader still receives every retained byte.
- A multi-reader test: two clients at different cursors both read
  correctly.
- A compile-level guard that the harness crate does not depend on the PTY
  dependency (workspace dependency check), so "the harness never gets a
  PTY" is structural, not disciplinary.
- A plausible wrong implementation persists ring contents to disk for crash
  recovery and passes every streaming test; the no-durable-bytes assertion
  must fail it.
