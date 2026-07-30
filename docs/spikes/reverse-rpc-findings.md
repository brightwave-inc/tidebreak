# Reverse-RPC callback channel — spike findings

Spike for [sandbox-providers.md](../sandbox-providers.md) step 5, issue #821. The
prototype lived in `crates/openwave-reverse-rpc-spike` (removed once its
findings were absorbed; recover it from git history) and was **not wired into
the server**. This document is the deliverable; the code exists to back its
claims with runnable tests.

## Verdict: GO

The host-held reverse-RPC callback channel is buildable at acceptable
complexity. Every hard part the design named — request/response correlation over
one multiplexed connection, durable per-run operation identity with recorded
responses, cancellation, backpressure, and disconnect-fails-inflight — is
demonstrated by a passing test against a real framed transport and a real
concurrent host and supervisor. None of them required exotic machinery; each
falls out of the same envelope discipline `openwave-host-broker` already uses.

The GO carries **a durable-storage condition #822 must discharge**, not a caveat
that can be deferred. It has two co-equal parts, both about the operation log:
(1) the operation-identity record and its recorded response must be *durable* and
committed in the right order relative to the model call's external effect; and
(2) that log must have a stated **retention** story, because a sandbox-resident
loop makes it a high-cardinality per-step structure rather than the bounded
receipt it resembles. The spike proves idempotency across a reconnect within a
single host process; it does **not** prove it across a host crash (part 1), and
its in-memory log is insert-only with no eviction (part 2). Both are known,
bounded pieces of work with a clear shape (below), not open research questions —
which is why the recommendation is GO rather than "GO if."

## What the prototype exercises

Its tests lived in `crates/openwave-reverse-rpc-spike/tests/reverse_rpc.rs`,
recoverable with the crate from git history.

| Property | Test | What it proves |
| --- | --- | --- |
| Idempotent replay across reconnect | `idempotent_replay_across_reconnect` | Same `OperationId` re-issued on a fresh connection returns the recorded answer; the model ran exactly once. |
| Operation-id conflict | `reissue_with_a_different_request_is_a_conflict` | Reusing an identity for a different request is refused, as the broker refuses a reused mutation identity. |
| Correlation | `concurrent_calls_correlate_to_their_own_responses` | Five overlapping calls over one connection each receive their own response, matched by `RequestId`. |
| Cancellation | `cancel_aborts_an_in_flight_request` | A cancel aborts the in-flight completion; the model's effect never finishes, and the caller sees `Cancelled`. |
| Backpressure | `backpressure_blocks_new_calls_until_the_host_drains` | With the host paused, a new call blocks on the in-flight bound instead of buffering, and unblocks only as the host drains. |
| Disconnect fails in-flight + safe re-issue | `disconnect_fails_inflight_and_reissue_returns_the_recorded_response` | A dropped connection fails the in-flight call with `Disconnected`; the detached execution keeps running and records; re-issue after reconnect returns the recorded answer, model still ran once. |
| Deny-by-default | `ungranted_capability_is_denied_by_default` | An ungranted capability is refused and never executes. |

## Transport choice

The spike runs the host and the sandbox supervisor as two `tokio` tasks joined
by a `tokio::io::duplex` pipe, speaking newline-delimited JSON frames
(`src/transport.rs`). This is the cheapest transport that still exercises the two
properties a real socket forces on the design:

- **Real framing.** A frame can span several underlying reads, and several
  frames can arrive in one read; `read_until` over a `BufReader` reassembles and
  retains the remainder, exactly as the shipped broker sidecar's hand-rolled
  newline protocol does. Nothing here relies on passing Rust structs through an
  in-process channel, which would have tested nothing about the wire.
- **Real concurrency.** Both ends make progress on the runtime at once, so
  correlation, out-of-order completion, and backpressure are genuine, not
  simulated by sequencing.

Dropping either half of the duplex models a dropped connection, which is how the
disconnect test induces a failure. No E2B/Daytona backend is stood up; per the
delivery sequence, the vendor seam is exercised at the protocol boundary later,
not in this spike.

For production the frame transport is an implementation detail behind the same
`Frame` enum: loopback TCP for a local container, a TLS-fronted stream for a
managed vendor. The design's direction rule (host dials, sandbox never dials)
is unaffected — the host still opens the connection; "reverse" refers only to
which side originates *requests* over it, and the spike models precisely that:
the supervisor originates requests, the host answers.

## Operation identity and recorded responses

This is the heart of the spike. Two identities travel on every request, mirroring
the broker's split between transport correlation and idempotency identity:

- `RequestId` — per-attempt, correlates one request frame with one response
  frame. A re-issue after reconnect uses a **new** `RequestId`.
- `OperationId` — durable per-run identity. A re-issue uses the **same**
  `OperationId`, and that is what makes the answer idempotent.

The host keeps an operation log keyed by `OperationId` (`src/host.rs`,
`CapabilityHost`). The whole idempotency rule is one function, `dispatch`:

- unknown identity → claim it, spawn the capability's execution **once**, return
  a handle to its outcome;
- known identity, same request → attach to the existing outcome without
  re-executing (this serves both a reconnect replay and a concurrent duplicate);
- known identity, different request → `OperationIdConflict`, the same refusal the
  broker gives a reused mutation identity with a different fingerprint.

The execution runs as a task **decoupled from the connection**: it writes its
result into a `watch` channel that the per-request forwarder reads. That
decoupling is what gives the disconnect semantics for free — a dropped
connection tears down the forwarder but not the execution, so the execution
finishes and records, and a later re-issue reads the record. An explicit cancel,
by contrast, fires a signal the execution selects on, so cancel *can* abort what
disconnect cannot.

### Where this persists in the real system

The spike's operation log is in-memory (`HashMap<OperationId, _>` behind a
`Mutex`) and its recorded response is a `watch` value. In production this record
is durable state on the **run**, and it maps cleanly onto machinery
[agent-runs.md](../agent-runs.md) and
[sandbox-providers.md](../sandbox-providers.md) already describe:

- The recorded outcome is an **immutable typed receipt keyed by the operation**,
  the same shape as the run tier's "immutable typed receipt keyed by the child"
  for final sandbox output, and the `WriteFile` mutation receipt in the broker
  (`operation_id` → terminal `Result`).
- The record's lifecycle is a small state machine — `Claimed(dispatched)` →
  `Recorded(response)` / `Failed` — persisted alongside the run row, under the
  same transactional serialization the run tier uses for durable transitions.
  The `OperationId` is the reverse-RPC analogue of the run's result idempotency
  key: it fences a re-issue the way the result key fences a duplicate result
  delivery.
- Reconnect draining already exists as a concept: the run's event stream resumes
  from a committed cursor. Recorded reverse-RPC responses are the request/response
  counterpart to that resumable event stream — the host commits a response once,
  keyed by `OperationId`, and a re-delivery is answered from the commit.

### Operation-log retention

The commit-ordering problem above is one of **two** durable-storage design items
#822 must own; this is the co-equal second, and neither the findings above nor
sandbox-providers.md currently addresses it.

The analogy to the run tier's "immutable typed receipt keyed by the child" holds
for *shape* but breaks on *cardinality*, and the break matters. That receipt is
bounded: one per attempt, a handful of children per run. The reverse-RPC
operation log is not. A sandbox-resident **loop** issues a reverse model-inference
request on every step, so a single run can produce thousands of distinct
`OperationId`s, each with a recorded `Response`. The spike's insert-only
`HashMap` retaining every full response body is therefore not a small version of
the durable structure — it is a fundamentally higher-cardinality, unbounded-per-run
one, and persisting it naively would grow the run row without limit and keep
every model completion the run ever made.

#822 must decide, explicitly, three things it can leave unstated today only
because the spike is in-memory and short-lived:

- **What bounds the log.** Is it size-bounded, count-bounded, or bounded by the
  run's own lifetime? A sandbox-resident run is already bounded by its absolute
  deadline, the provider lifetime cap, and the token lifetime (invariant 5), so
  the log's worst case is finite — but "finite" at thousands of entries times a
  full response body each is still a real storage cost that needs a stated cap.
- **When a `Recorded` entry can be evicted.** The idempotency guarantee only has
  to hold while the sandbox can still re-issue that `OperationId`. Once the
  supervisor has acknowledged consuming the response — the same
  consume-and-advance-a-cursor shape the event stream already uses — that
  `OperationId` can never be re-issued, so its recorded entry is safe to evict.
  This argues for an acknowledgement/cursor protocol on the reverse channel, not
  an unbounded log: retention is keyed to un-acknowledged operations, not to the
  whole run history.
- **Whether the full `Response` body must be retained, or just a commit marker.**
  Replay requires returning the recorded response *body*, so an un-acknowledged
  entry must keep it. But an entry that must be retained past acknowledgement for
  audit or accounting may only need a commit marker (`OperationId` + outcome
  class + a spend/audit reference), not the completion text. Splitting "retained
  for replay" from "retained for audit" is what keeps the durable log from
  carrying every model completion verbatim for the run's whole life.

This is a scoping addition, not a design the spike solves. The point is that the
durable operation log is a higher-cardinality structure than the receipt it
resembles, and #822 owns its retention story alongside its commit ordering.

## Cancellation and backpressure

**Cancellation** (`Call::cancel` → `CancelFrame` → `CapabilityHost::cancel`)
fires a one-shot the execution's `select!` is waiting on, so the in-flight model
future is dropped rather than run to completion. The test asserts the model's
effect counter never advances after cancel even once a permit is offered — the
work was genuinely interrupted, not merely reported cancelled. Cancellation is
recorded as a terminal `Cancelled` outcome, so a later re-issue of a cancelled
operation returns `Cancelled` rather than starting a second execution. That
choice deliberately matches the design's conservative stance: a model call with
possible partial spend is not silently retried.

**Backpressure** has two layers on the supervisor:

- an in-flight **semaphore** (`max_in_flight` permits); `call` awaits a permit
  before it will even register a request, and the permit is held by the live
  `Call` until its result is taken. When the host stops answering, permits are
  not returned, and new calls block.
- a **bounded outbound queue** feeding the writer task, so the write path cannot
  buffer past its capacity either.

The test pauses the host, saturates the two permits, and asserts a third call
does not complete within a timeout — then releases the host and watches the
third call proceed as permits free. This is backpressure keyed to consumer
progress, not a fixed-size buffer that eventually drops: the sandbox is bounded
by how fast the host drains, which is what the design requires of a slow or
paused host.

## Failure modes hit, and honest limits

- **Cancel shares the outbound queue.** `Call::cancel` uses a non-blocking
  `try_send`; under a full queue the cancel is dropped. A production channel
  needs a **reserved control lane** for cancel (and heartbeats) so a cancel is
  never blocked behind the very backlog it is trying to relieve. This is a small
  protocol addition, flagged for #822, not a design flaw.
- **In-flight dedup is process-local.** The spike attaches a reconnect re-issue
  to the *same running future*. Across a host crash that future is gone. The
  durable record's `Claimed` state must therefore be the source of truth, and a
  re-issue that finds `Claimed` after a crash **cannot safely re-execute** a
  call with an external effect — it fails conservatively, matching the
  repository-wide rule that ambiguous execution is failed, never replayed. The
  spike does not implement crash durability; it demonstrates the reconnect path
  the durable record would sit under.
- **The operation log only grows.** The spike's log is a
  `HashMap<OperationId, Arc<OperationEntry>>` that only ever inserts and retains
  the full recorded `Response` per entry. That is fine for a spike but wrong for
  a real run: it has no retention, eviction, or GC, and its cardinality is
  fundamentally higher than the receipts it is analogized to (see the retention
  section below).
- **Single capability.** Only model inference is carried, as the issue scopes.
  The `ReverseRequest`/`ReverseResult` enums are `#[non_exhaustive]` and adding a
  capability (host search, consent prompt) is a variant plus a handler; the
  gating and idempotency paths are capability-agnostic.
- **Trust and transport authenticity are out of scope.** Per the doc's trust
  model, a managed vendor's control plane can impersonate the run; the spike
  does not defend against that and is not meant to. Per-run transport secret,
  TLS identity pinning, and loopback pinning are transport concerns for the
  protocol step.

## Complexity and risk assessment

Complexity is **moderate and bounded**. The prototype is ~4 small modules of
ordinary async Rust with no new dependency beyond `tokio` features already in the
workspace pin. The structural idea that carries the weight — split run-scoped
state (grants, operation log, model proxy) from connection-scoped state
(framing, forwarders) — is the same host/connection split the broker already
draws between its `Controller`/`Operator` surfaces and its sidecar. There is no
event loop to hand-roll, no custom executor, no unsafe.

The **single biggest risk** is durable operation-identity commit ordering
against the model call's external effect. Get the commit predicate wrong and you
either double-spend (re-execute a call that already ran) or lose work
(fail a call that could have been recorded). It is the same class of problem the
run tier already solved for result commit with an idempotency key and a
single-predicate check, so the pattern to copy exists — but it must be built
deliberately at the protocol step, not assumed. Its co-equal companion is the
operation log's **retention** story (above): a sandbox-resident loop makes the
durable log a high-cardinality, per-step structure, so #822 owns both how the
log commits and how it is bounded and evicted. Everything else the spike touched
was straightforward.

Had the spike been NO-GO, the design already names the cost:
sandbox-resident runs would require a scoped-token issuer **even while
attached**, so an install whose only model credential is a long-lived provider
key could not run them at all; and host-mediated capabilities (host search,
consent prompts) would be absent from sandbox-resident runs entirely. That is a
hard product regression, and the spike's result avoids it: attached
sandbox-resident runs can proxy inference through the host.

## What #822 (the protocol step) must nail down

1. **Durable operation record + commit predicate.** Persist the operation log on
   the run: `Claimed(dispatched)` → `Recorded(response)` / `Failed`, committed
   transactionally, keyed by `OperationId`. Define the commit predicate so a
   re-issue is answered from `Recorded`, a `Claimed`-after-crash call with an
   external effect fails conservatively rather than re-executing, and the record
   is the run's, surviving any single connection. This is the biggest risk and
   the gating design decision.
2. **Operation-log retention** (see the retention section). Co-equal with item 1:
   bound the durable log, define when a `Recorded` entry is evictable (an
   acknowledgement/cursor once the sandbox can no longer re-issue the
   `OperationId`), and decide whether an entry retains the full `Response` body
   or only a commit marker. The log is per-step and unbounded-per-run without
   this, unlike the bounded receipt it resembles.
3. **A reserved control lane** for cancel and liveness, separate from the
   request/response backlog, so cancellation and heartbeats are never subject to
   request backpressure.
4. **Version negotiation** on connect, following the broker's exact-equality
   `PROTOCOL_VERSION` check (the spike checks equality per request; the protocol
   should also handshake on attach, as the broker's `Hello` does).
5. **Capability grant provenance.** The spike takes a static grant set; the real
   host resolves grants per run at admission, deny-by-default, and audits each
   reverse operation with run provenance, as the reachability section requires
   for consent prompts.
6. **Bounded results and frame-size limits** carried into the protocol
   explicitly (the spike bounds frame size; the protocol should state per-capability
   result bounds as the broker states `MAX_*` limits).
