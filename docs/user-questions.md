# Durable user questions

The foreground coordinator can pause its current turn with
`ask_user_questions` when it needs a small, explicit choice before continuing.
This is a durable continuation, not a new chat message and not a native client
tool: the answer completes the original model tool call and makes that same
turn claimable again.

Sandbox agents and direct/non-coordinator agent surfaces never receive this
tool. A sandbox that needs clarification must return its finding to the
foreground coordinator, which decides whether to ask the user.

## Product flow

1. The model emits `ask_user_questions` alone, without assistant text or sibling
   calls.
2. Under the live turn lease, OpenWave atomically stores the bounded questions,
   the pending tool call, the client-wait checkpoint, and a renderer refresh
   event. The worker releases its lease in `waiting_for_client`.
3. The desktop loads the durable card from
   `GET /chats/{chat_id}/questions/pending`. A WebSocket event and native
   attention request only reduce latency; polling and the database projection
   remain authoritative after reload or process restart.
4. The user answers every question once through
   `POST /chats/{chat_id}/questions/{call_id}/answer`.
5. One transaction stores the exact answers, completes the tool call with the
   model-facing JSON result, journals the call's `ToolCallCompleted`, closes
   the wait, and moves the original turn to `resuming`. The route publishes the
   completion live so the renderer settles the card immediately instead of at
   the turn's terminal hydration. A worker then reclaims that turn and
   reconstructs the matching `ToolUse` and `ToolResult` in the provider
   transcript.

Cancelling the turn closes an unanswered card and the wait in the same
serialized state transition. Answer and cancellation take the same chat, turn,
and call locks, so exactly one wins. A card can never revive after a terminal
turn.

## Closed contract

One call contains one to three questions. Every question has a stable ID, short
header, prompt, up to five mutually exclusive options, and an optional
free-form alternative. IDs are unique within their scope. Text fields and the
answer body are bounded, unknown fields are rejected, and a question must offer
at least one option or opt into free-form input. Presentation fields reject
control characters; free-form answers permit line breaks and tabs but reject
other controls.

An answer must cover every question exactly once and provide exactly one of:

- a valid option ID; or
- a non-empty free-form answer when that question permits one.

Semantic validation failures return `400`. Repeating the exact committed answer
returns `200` with `{"disposition":"existing"}`. A contradictory retry or an
answer racing behind cancellation returns `409`. The answer route is capped at
8 KiB independently of the semantic limits.

## Renderer and trust boundary

The renderer DTO contains only `call_id`, `turn_id`, `asked_at`, and the
validated presentation fields. The live event contains only the call and turn
IDs. Raw provider metadata, tool arguments, executor identities, leases, host
paths, and diagnostics never cross this boundary.

Question calls are stored as foreground orchestration records. They are not
eligible for the server tool executor or the desktop's generic native executor.
The dedicated answer command is the only successful resolution path.

Native attention is explicitly best effort and requested only while the desktop
window is unfocused. It carries no question content and is never required for
correctness.

## Maintainer checks

Changes to this path should preserve:

- atomic question, wait, event, and tool-call checkpointing;
- a single renderer completion announcement committed with the answer, never
  repeated by an exact retry;
- exact idempotent answer recovery with contradictory retries rejected;
- serialization of pending projection reads with answer and cancellation;
- deletion ordering for question rows before their chat, turn, tool, and event
  parents;
- identical lifecycle behavior on SQLite and PostgreSQL;
- strict renderer decoding and the foreground-only model surface.

The SQLite suite exercises restart recovery, answer/cancel and projection races,
chat deletion, exact retries, and model transcript reconstruction. The
PostgreSQL state-machine suite repeats answer, worker reclaim, and cancellation
race coverage when `OPENWAVE_POSTGRES_TEST_URL` is configured.
