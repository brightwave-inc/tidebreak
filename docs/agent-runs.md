# Agent runs and sandboxed background work

OpenWave separates the conversation a person sees from the durable agents that
do its work. This keeps the foreground chat responsive while longer tasks run
asynchronously, and it gives every background task an isolated place to work.

The initial design is deliberately bounded:

- each chat has a foreground agent run;
- the foreground agent may start background agents;
- every background agent runs in a sandbox;
- background agents cannot start more agents (`depth <= 1`);
- configurable global and per-chat limits bound queued and running agents.

This is enough for useful parallel research and production work without needing
a recursive fleet scheduler.

## The execution hierarchy

```text
Chat
└── foreground AgentRun (depth 0)
    ├── TurnRun
    ├── background AgentRun (depth 1, sandboxed)
    └── background AgentRun (depth 1, sandboxed)
```

An `AgentRun` is the durable execution context. It owns lifecycle, parentage,
execution location, budgets, and result delivery. A `TurnRun` is a foreground
conversation segment inside that context. Model steps, tool calls, waits, and
receipts are resumable work beneath the run rather than independent ad-hoc
tasks.

Chat creation and foreground-run creation are one atomic database operation.
The foreground identity is derived from the chat identity, so retries and every
process agree on the same coordinator without a lookup race. Turn acceptance
stores a database-enforced reference to that exact depth-zero run; a turn can
never be admitted under another chat's coordinator or a background child.

Foreground and background agents use the same loop mechanics: durable
acceptance, exact claim ownership, model and tool checkpoints, steering,
cancellation, ordered events, and fenced completion. Their product contracts
differ:

| Foreground agent | Background sandbox agent |
| --- | --- |
| Responds directly in the chat | Works on one delegated task |
| Final assistant text completes a turn | An explicit result submission completes the run |
| Uses conversation-facing tools | Uses sandbox, project, and workspace tools |
| Usually short-lived work | May park and resume over a longer period |
| Streams answer content | Publishes bounded progress and a final result |

This shared foundation is important. OpenWave must not grow separate,
incompatible foreground and sandbox loops.

## Starting background work

Starting an agent is an asynchronous state transition, not an in-process task
spawn:

1. The foreground tool call atomically creates a queued child `AgentRun`.
2. The tool-call identity is stored as the child's unique spawn identity, so an
   ambiguous retry recovers the original run instead of creating a duplicate.
3. A bounded scheduler claims the child with an exact renewable lease.
4. The scheduler creates or restores its sandbox and advances the shared agent
   loop.
5. The child submits an immutable terminal result or the scheduler records a
   durable terminal failure.
6. The result, terminal child state, and immutable parent inbox entry commit
   together. A later wait transition may consume that entry and wake a parent.

The bounded `spawn_sandbox_agent` contract and its foreground checkpoint wiring
are prepared, but deliberately disabled in the production tool registry: no
sandbox executor exists yet to claim and complete the accepted child. When that
executor lands, this will be a wait-form tool: it accepts one bounded `task`,
derives the child identity from that exact tool call, and commits the queued
child, durable wait, and release of the exact foreground worker lease in one
transaction. It will be advertised only to a claimed foreground turn; sandbox
workers will never receive it, and the store independently enforces depth one.
A later non-blocking spawn tool can let the foreground continue after spawning,
but must earn the same checkpoint and replay rules. The prepared boundary never
leaves a child behind when a stale lease or steer fences its checkpoint.

The wait also carries an immutable atomic-admission marker. Only that receipt
allows a later retry to recover the combined transition; an older path that
accepted a child and parked a turn in separate commits is never mistaken for
proof that both effects committed together.

## One continuation model

Anything that cannot finish as an ordinary bounded function call uses a common
durable continuation mechanism. That includes:

- client-side tool execution;
- approval of a sensitive operation;
- a request for another host folder;
- a question that needs a user response;
- waiting for a document or another resource;
- waiting for one or more background agents.

A wait records the exact dependency and checkpoint, releases worker ownership,
and becomes claimable again only after a durable receipt resolves it. A process
notification may reduce latency, but it is never the source of truth.

## Sandbox and host-access boundary

A background agent receives private scratch space. Access to a project or host
folder is capability-based and mediated by the host broker described in
[Host access and connected folders](host-access.md). The model sees opaque root
identities and root-relative paths, never unrestricted absolute host paths.

An agent may request additional access, but only the trusted native host can
show the picker and register the user's selection. The sandbox parks while that
request is unresolved and resumes from its durable checkpoint afterward.

The sandbox boundary should remain useful outside the desktop product. A local
process sandbox is the first execution adapter; self-hosted and managed profiles
may later supply stronger isolation behind the same contract.

## Bounded scheduling

Concurrency control has three independent purposes: protecting the machine,
keeping one chat from monopolizing it, and preventing an agent from creating an
unbounded queue. The scheduler therefore applies configurable limits to:

- background agents running across the installation;
- running and outstanding children per chat or foreground run;
- total children created by one foreground run;
- wall-clock time, model steps, tool calls, and other resource budgets.

When a running slot is unavailable, accepted work remains durably queued. A
waiting agent does not consume a running slot. Cancellation of a foreground run
also cancels its owned queued or running children in the initial model; detached
agents are intentionally deferred.

The scheduler's ownership rules are deliberately strict:

- a singleton durable scheduler lock makes global and per-chat running caps
  atomic across local processes;
- scheduler ownership uses the database clock rather than a worker-supplied
  timestamp, so a skewed local process cannot steal a live lease;
- every claim has a unique receipt, including an empty scan, so retrying a
  claim token can recover only its original running lease or its original
  no-work result;
- a running lease has a monotonically extendable expiry and never outlives the
  run's absolute wall-clock deadline;
- an expired lease starts a new attempt only while budget remains; exhausted
  attempts and deadlines become durable terminal failures, while an expired
  cancellation fence becomes `cancelled` and releases capacity;
- a worker's writes must carry its exact live lease token, so a reclaimed or
  expired worker cannot resume ownership.

Final sandbox text is stored as an immutable receipt keyed by the child run and
the exact lease segment that submitted it. The receipt, `completed` state, and
one parent inbox entry commit together, so an ambiguous worker retry can recover
its original result but cannot overwrite or double-deliver it. Each inbox entry
then advances through its own fenced continuation state machine:
`pending -> claimed -> consumed`. A parent continuation claims one exact child
result only after the matching foreground checkpoint has committed, with a
database-clock lease, and only that live lease can consume it. A result that
arrives first remains pending; it is never leased merely because a process saw
it before the parent reached its durable boundary.
An expired claim can be reclaimed; a consumed entry retains the exact consuming
lease as an immutable receipt, so an ambiguous consume retry recovers the same
boundary without processing the child result twice. A foreground worker can
checkpoint its exact turn against a known child, releasing its turn lease and
preserving model progress. Consumption then closes that checkpoint and moves
the turn to `resuming` in the same transaction. `resuming` is the durable wake
signal: any worker can claim it after restart, without relying on an in-memory
notification. Queued, waiting,
and retry-wait work cancels
immediately; a running worker first enters `cancelling` and must acknowledge its
exact live lease. That acknowledgement writes its own immutable receipt, so only
the worker that actually committed terminal cancellation can recover an
ambiguous retry. An expired running lease is cancelled immediately on request
rather than reclaimed. Parent inbox delivery is intentionally the next
transition, not a process-local notification.

## Observing execution

The authenticated local API exposes `GET /chats/{id}/agent-runs` for a
chat-scoped snapshot of its foreground coordinator and any sandbox children.
It is a read model, not a scheduler control surface: clients use it to render
queued, running, waiting, failed, and completed work, while workers continue to
advance runs solely through fenced store transitions. A missing chat returns
`404`, rather than revealing whether an unrelated run identifier exists.
The response is deliberately renderer-safe: worker lease tokens, delegated
input, and scheduler bookkeeping never cross this API boundary.

## Reliability contract

The agent hierarchy preserves the runtime's existing rules:

1. Accepted work is already durable.
2. One exact lease owner advances a run at a time.
3. Stale workers cannot publish messages, effects, results, or events.
4. Every externally visible tool effect has a stable call identity and terminal
   receipt before it becomes safely retryable.
5. Waits release workers and resume from committed checkpoints.
6. Steering and cancellation are resolved at explicit boundaries.
7. A final result, terminal run state, and immutable parent inbox entry are
   committed atomically. A foreground turn may checkpoint against one exact
   inbox delivery. When the child is spawned from a wait boundary, that
   checkpoint commits with child admission and releases the foreground lease;
   consuming the delivery under an exact expiring continuation lease also wakes
   the checkpointed turn to `resuming`, with exact retry recovery.
8. Clients recover from a durable snapshot plus ordered event replay.

Until a tool satisfies the side-effect receipt contract, OpenWave continues to
fail conservatively after an ambiguous execution rather than replay it.

## Delivery sequence

The implementation is intentionally incremental:

1. Add the durable `AgentRun` hierarchy, atomic foreground ownership, and
   depth-one constraints. *(Shipped.)*
2. Add the bounded sandbox scheduler and lease lifecycle. *(Shipped.)*
3. Add idempotent cancellation and immutable fenced result submission.
   *(Shipped.)*
4. Add the parent inbox and atomic child-result delivery. *(Shipped.)*
5. Generalize client execution into the shared continuation model.
6. Persist shared model/tool step boundaries and side-effect receipts.
7. Atomically join durable inbox consumption with a parent turn checkpoint and
   durable `resuming` wake signal. *(Shipped.)*
8. Atomically accept a sandbox spawn and its parent checkpoint from the
   foreground tool boundary. The bounded foreground-only
   `spawn_sandbox_agent` contract is wired to this transition, but remains
   disabled until the sandbox executor lands. *(Groundwork shipped.)*
9. Route sandbox folder access through the host broker.
10. Add desktop surfaces for queued, running, waiting, failed, and completed
   background work.
11. Add richer context lifecycle, parallel-safe tool groups, and further
   orchestration only after these recovery boundaries are proven.
