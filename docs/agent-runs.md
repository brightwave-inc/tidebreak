# Agent runs and sandboxed background work

OpenWave separates the conversation a person sees from the durable agents that
do its work. This keeps the foreground chat responsive while longer tasks run
asynchronously, and it gives every background task an isolated place to work.

The initial design is deliberately bounded:

- each chat has a foreground agent run;
- the foreground agent may start background agents;
- every background agent runs in a sandbox;
- background agents cannot start more agents (`depth <= 1`);
- at most four children from one foreground turn may remain unsettled;
- configurable global and per-chat limits bound actively running agents.

This is enough for useful parallel research and production work without needing
a recursive fleet scheduler.

From the user's point of view, the foreground assistant can hand several
independent jobs to background workers, continue organizing the task, and then
pause at one explicit join point. Results return together in the order the
assistant requested, even if the workers actually finished in another order.
Closing the app or restarting the server does not lose the accepted jobs or
their place in the conversation.

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
claimed foreground turn. One bounded `task` derives a child identity from its
exact tool call. In one transaction, OpenWave admits the queued child, records
the completed orchestration tool result containing its opaque `agent_id`,
journals that completion, applies model progress once, and releases the
foreground lease into `resuming`. The foreground can therefore be reclaimed
and continue while the child runs. Only after the commit does the server wake
the foreground and sandbox workers. Those notifications reduce latency; their
durable scans remain the correctness path after a missed wake or restart.

Each spawn call is made alone and returns one ID for the foreground agent to
retain. Up to four children from the exact origin turn may be unsettled at once.
"Unsettled" includes both live work and a terminal result that has not yet been
consumed, so finishing quickly does not accidentally open an unbounded queue.
A consumed result, or one explicitly retired by cancellation, releases its
slot. Sandbox workers never receive either orchestration tool, and storage also
enforces depth one.

The paired `wait_for_agents` call accepts one to four unique child IDs in caller
order and has only `All` completion semantics. The call is also made alone. Its
park transaction proves every ID belongs to the exact origin turn, records the
pending orchestration tool call and ordered membership, applies model progress
once, and moves the foreground turn to `waiting_for_agent_run`. Scheduler
leases, executor identities, and continuation tokens never cross this
model-facing boundary.

When every child has a terminal inbox delivery, one resume transaction consumes
all of them exactly once, completes the same tool call with typed results in
request order, appends the exact `ToolCallCompleted` journal event, and changes
the foreground turn to `resuming`. A child may finish before the wait is parked;
the durable readiness scan still finds it. If a steer interrupts an open wait,
the wait call closes but the children and their deliveries remain available for
a later wait. Parent cancellation instead fences its owned children and retires
unconsumed delivery.

Foreground completion has a final storage guard. If the model tries to return a
final answer while any admitted child remains unsettled, the completion does
not commit. The worker continues with the complete ordered ID list and directs
the model to issue one valid wait. This prevents a prompt mistake or premature
provider response from orphaning background work.

Ambiguous responses keep the original identity. Spawn recovery reads its
immutable checkpoint before mutable lease or steer state, so a lost response
cannot create a second child or apply usage twice. Ordered-wait recovery keeps
one resume token across transient adapter errors; if the first response was
lost after commit, the exact retry recovers the already-consumed results and
the same journal event. That event may be published live again, but it retains
its durable sequence number. Reconnecting clients replay the journal, and live
clients deduplicate by sequence cursor.

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

A background agent has private runtime scratch but cannot access that scratch,
projects, host folders, the general network, or the parent conversation. Its
current narrow exceptions are one checkpointed public web search or a typed
folder-access proposal that grants no access. It never receives the foreground
spawn/wait or broker tool contracts. Foreground chats may inspect already
attached roots, but sandbox agents do not receive the broker transport. Future
sandbox folder access must use the parent-mediated consent and
durable-receipt protocol described in [Host access and connected folders](host-access.md),
never absolute paths.

The sandbox boundary should remain useful outside the desktop product. A local
process sandbox is the first execution adapter; self-hosted and managed profiles
may later supply stronger isolation behind the same contract.

## Bounded scheduling

Concurrency control has three independent purposes: protecting the machine,
keeping one chat from monopolizing it, and preventing an agent from creating an
unbounded queue. The scheduler therefore applies configurable limits to:

- background agents running across the installation;
- running children per chat;
- four unsettled children per foreground turn, including delivered results
  that have not yet been consumed;
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

Final sandbox output is stored as an immutable typed receipt keyed by the child
run and the exact lease segment that submitted it. Besides final text, the only
current non-text outcome is a validated folder-consent proposal. It carries no
host path, root identity, broker grant, or client-call identity; it only tells
the foreground parent to decide whether the existing foreground consent tool is
appropriate. The receipt, `completed` state, and one parent inbox entry commit
together, so an ambiguous worker retry can recover its original result but
cannot overwrite or double-deliver it. Each inbox entry advances through a
fenced lifecycle: `pending -> consumed`, with a stable resume token proving the
consumer, or `cancelled` when the parent retires the delivery. The ordered wait
consumes all named entries in one transaction only after the matching
foreground checkpoint exists. A result that arrives first remains pending; it
is never treated as parent-visible merely because a process noticed it early.

The completed wait tool result is the model-facing delivery. It contains each
typed child result in the caller's original order and is reconstructed from the
same immutable receipts after restart. `resuming` is the durable wake signal:
any foreground worker can claim it without relying on an in-memory
notification.

Queued, waiting, and retry-wait child work cancels immediately. A running child
first enters `cancelling` and must acknowledge its exact live lease. That
acknowledgement writes its own immutable receipt, so only the worker that
committed terminal cancellation can recover an ambiguous retry. An expired
running lease is cancelled immediately on request rather than reclaimed.
Cancelling a foreground turn fences its owned children in the same ordered
transition: queued work is cancelled, running work is asked to acknowledge,
and delivered inbox receipts are retired. Inbox delivery and consumption are
always durable state transitions, never process-local notifications.

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

When an agent has a live, supported tool checkpoint, the snapshot may also
contain a small `activity` object. Its values are a deliberately admitted,
fixed display vocabulary—for example, sandbox `web_search` or foreground
`list_connected_folders`, `list_folder`, and `read_connected_file`, each with
`waiting` or `running` status. It is not a tool trace: queries, tool arguments,
results, folder/root identities, relative paths, filenames, host paths, grants,
provider identifiers, executor leases, and raw failures remain server-side.
New tools are invisible to the renderer until they receive their own safe
activity projection.

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
   committed atomically. Spawn separately commits child admission with its
   completed orchestration result and foreground `resuming` transition. An
   ordered wait checkpoints one to four exact child IDs; consuming all matching
   deliveries completes that same tool call and wakes the foreground turn to
   `resuming`, with exact retry recovery at both boundaries.
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
9. Prove the bounded foreground-only spawn path with atomic child admission,
   sandbox wake-up, immutable delivery, and parent resume. *(Shipped.)*
10. Add the one-call sandbox `web_search` checkpoint, host executor, and
    receipt-backed model resume. *(Shipped.)*
11. Let a sandbox relay a typed folder-consent proposal to its foreground
    parent, without host access or a picker. *(Shipped.)*
12. Add a fenced read-only proxy for roots the foreground chat already
    attached; sandbox broker access remains deferred.
13. Add desktop surfaces for queued, running, waiting, failed, and completed
   background work.
14. Persist the non-blocking spawn checkpoint and ordered wait receipts.
    *(Shipped.)*
15. Activate non-blocking spawn and ordered multi-agent waits together.
    *(Shipped.)*
16. Add richer context lifecycle, parallel-safe tool groups, and further
   orchestration only after these recovery boundaries are proven.
