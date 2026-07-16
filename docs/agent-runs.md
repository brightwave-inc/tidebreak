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
| Uses conversation-facing tools | Starts with no tools or shared conversation context |
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
4. The scheduler gives the child private scratch and advances its isolated
   no-tools task loop.
5. The child submits an immutable terminal result. Provider failures leave the
   exact lease for the bounded scheduler retry/reap path; they do not create an
   unfenced executor-side failure transition.
6. The result, terminal child state, and immutable parent inbox entry commit
   together. A later wait transition may consume that entry and wake a parent.

The bounded `spawn_sandbox_agent` contract is available only to a durably
claimed foreground turn. It is a wait-form tool: one bounded `task` derives a
child identity from its exact tool call, then commits the queued child, durable
wait, and release of the foreground lease in one transaction. Only after that
commit does the server wake the bounded sandbox worker. The notification is a
latency hint, not a correctness dependency: the worker polls durable child and
inbox state, so a missed wake or restart cannot lose accepted work. A sandbox
result is delivered, claimed under an exact continuation lease, consumed, and
turns the parked foreground turn into a fresh durably claimable `resuming`
attempt. The tool is never advertised to sandbox workers, and the store
independently enforces depth one. A later non-blocking spawn tool can let the
foreground continue after spawning, but must earn the same checkpoint and
replay rules.

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

A background agent has private runtime scratch, but the initial executor has no
tools and cannot access that scratch, projects, host folders, the network, or
the parent conversation. Broker-mediated folder access is a future capability;
when introduced it must use the consent and durable-receipt protocol described
in [Host access and connected folders](host-access.md), never absolute paths.

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
`pending -> claimed -> consumed` (or `cancelled` when its parked parent is
cancelled). A parent continuation claims one exact child
result only after the matching foreground checkpoint has committed, with a
database-clock lease, and only that live lease can consume it. A result that
arrives first remains pending; it is never leased merely because a process saw
it before the parent reached its durable boundary.
An expired claim can be reclaimed; a consumed entry retains the exact consuming
lease as an immutable receipt, so an ambiguous consume retry recovers the same
boundary without processing the child result twice. Consumption also persists
the exact terminal child text as a deterministic system transcript message, so
the resumed model request sees it after a restart. A foreground worker can
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
rather than reclaimed. Cancelling a parked parent fences its owned sandbox
child in the same durable transition: queued work is cancelled, running work
is asked to acknowledge cancellation, and a delivered inbox receipt is retired.
Parent inbox delivery is intentionally the next transition, not a process-local
notification.

## Observing execution

The authenticated local API exposes `GET /chats/{id}/agent-runs` for a
chat-scoped snapshot of its foreground coordinator and any sandbox children.
It is a read model, not a scheduler control surface: clients use it to render
queued, running, waiting, failed, and completed work, while workers continue to
advance runs solely through fenced store transitions. A missing chat returns
`404`, rather than revealing whether an unrelated run identifier exists.
The response is deliberately renderer-safe: worker lease tokens, delegated
input, raw failure details, and scheduler bookkeeping never cross this API
boundary. A bounded failure code may be included for display and recovery
guidance; detailed provider, transport, or executor diagnostics remain
server-side.

When a sandbox has a live, supported tool checkpoint, the snapshot may also
contain a small `activity` object. Its values are a deliberately admitted,
fixed display vocabulary—for example, `web_search` with `waiting` or
`running` status. It is not a tool trace: queries, tool arguments, results,
provider identifiers, executor leases, and raw failures remain server-side.
New sandbox tools are invisible to the renderer until they receive their own
safe activity projection.

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
8. Run isolated sandbox tasks with no shared conversation context, over the
   durable lease/result boundary. *(Shipped.)*
9. Enable the bounded foreground-only `spawn_sandbox_agent` contract, wake the
   sandbox worker after its atomic checkpoint, and resume the parked turn from
   the already-delivered inbox receipt. *(Shipped.)*
10. Add the one-call sandbox `web_search` checkpoint, host executor, and
    receipt-backed model resume. *(Shipped.)*
11. Route sandbox folder access through the host broker.
12. Add desktop surfaces for queued, running, waiting, failed, and completed
   background work.
13. Add richer context lifecycle, parallel-safe tool groups, and further
   orchestration only after these recovery boundaries are proven.
