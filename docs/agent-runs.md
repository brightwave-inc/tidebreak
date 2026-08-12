# Agent runs and sandboxed background work

OpenWave separates the conversation a person sees from the durable agents that
do its work. This keeps the foreground chat responsive while longer tasks run
asynchronously, and it gives every background task an isolated place to work.

The initial design is deliberately bounded:

- each chat has a foreground agent run;
- the foreground agent may start background agents;
- every background agent runs in a sandbox;
- background agents cannot start more agents (`depth <= 1`);
- at most four children from one foreground turn may remain unsettled
  (`wait_for_agents` membership, spawn admission, and
  `max_active_background_agents` share that ceiling so a turn cannot hold more
  unsettled children than one wait can consume);
- configurable global and per-chat scheduler limits bound agents that are
  actually running.

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
| Uses conversation-facing tools | Uses a small fixed sandbox tool surface over a bounded checkpoint budget; has no shared conversation context |
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
4. The scheduler gives the child private scratch and advances its isolated task
   loop with only the narrow sandbox-safe tool surface.
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

A spawn may also delegate one exact file as an opaque root ID plus a non-empty
root-relative path. Admission verifies that the root is attached to the
foreground chat and stores the resource immutably with that child. This does
not grant a folder, expose a host path, or let the child choose another target;
it only makes the argument-free desktop read described below eligible while
the same attachment remains current.

Each spawn call is made alone and returns one ID for the foreground agent to
retain. Up to four children from the exact origin turn may be unsettled at once
— the same bound `wait_for_agents` accepts and the default
`max_active_background_agents` setting. "Unsettled" includes both live work and
a terminal result that has not yet been consumed, so finishing quickly does not
accidentally open an unbounded queue. A consumed result, or one explicitly
retired by cancellation, releases its slot. Sandbox workers never receive either
orchestration tool, and storage also enforces depth one.

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

`ask_user_questions` now uses this continuation boundary directly. It
atomically stores a bounded renderer projection with the client wait, releases
the foreground lease, and resumes the same turn only after an exact answer is
committed. Exact retries recover the prior answer; contradictory retries and
answer/cancel races cannot produce two results. See
[Durable user questions](user-questions.md).

## Sandbox and host-access boundary

A background agent cannot access projects, general host folders, the general
network, or the parent conversation. Its narrow exceptions share one bounded
tool-call budget: commands in its own execution workspace, a checkpointed
public web search, a typed folder-access proposal that grants no access, or—only
in the embedded desktop—one exact file named by its immutable admission. A run
may spend that budget over several checkpoints; the worker replays the whole
resolved chain on every claim, which is why the count is capped. The read tool
takes no arguments. A native executor revalidates the current chat attachment
immediately before the host broker performs a bounded UTF-8 read, persists
private no-replay recovery state, and publishes only bounded content or a
neutral failure. Headless workers never advertise the read. Sandboxes do not
receive foreground spawn/wait or broker transport contracts, and absolute paths
never cross into provider context. See
[Host access and connected folders](host-access.md).

Execution is how a background run produces anything the user keeps. Its
workspace is named by the run, not the conversation, so siblings delegated in
one message never share a filesystem; it starts empty and holds nothing but what
the run's own earlier commands wrote. The request carries no folder authority
and stages no host paths — delegation already bypasses the conversation's
approval gate, so this path must not be the one that hands a delegated agent the
user's files. The one thing the parent conversation contributes is its network
policy, because the user chose that policy for this work. After each command the
host scans the workspace's `output/` and publishes what it finds into the parent
conversation's outputs, keyed by filename and attributed to the run: a new
filename becomes a new output, and the same filename written again becomes its
next version. The agent names its own deliverables that way, and the host never
invents a title for one. See [Outputs](deliverables.md).

This is intentionally not chat-wide or root-wide access. A sandbox cannot list
roots or directories, open a picker, request a different file, write outside its
own workspace, or use a general filesystem API. A detach can revoke the exact
read before claim, at the final pre-dispatch heartbeat, or at resolution; if
content arrived after authority was lost, it is discarded rather than returned
to the model.

A background run also keeps an ordered checklist of its own. `update_task_plan`
is a checkpoint like the others, not a local note: the row lives in the host's
database, so the call parks, the host resolves it, and the run reads the result
back on its next step. Each call replaces the whole list, and the plan is keyed
by the run rather than by the chat — four siblings delegated in one message are
working four different tasks, and a chat-keyed row would have them overwriting
each other and the conversation's own plan.

Plan rows are budgeted apart from the rest. A run is told to keep its checklist
current as steps finish, which is a call after most real steps; charged to the
same tool allowance, bookkeeping would starve the commands and searches the task
is actually for, and the run would exhaust itself describing work it never got
to do. So the allowance above bounds work rows, and plan rows get their own
smaller cap — enough revisions to narrate one delegated task, few enough to
bound a model that does nothing else. Each budget withdraws its own tools when
it runs out, and the durable store enforces the same split, so the advertised
surface and the bound the transaction applies cannot disagree. Model steps are
unaffected: every checkpoint still costs the completion that makes it and the
completion that reads its result.

When a run calls `done` with steps still open, the host hands that call back
once with the open steps named, the same way it answers a terminal tool that
arrived with company. It is a reminder, not a gate: the second `done` submits,
the push-back is spent by its own receipt code rather than by the presence of
any earlier `done`, it is withheld when the run cannot afford the work row and
the step it costs, and a run that never made a plan is never interrupted. A run
that ends by producing final text rather than by submitting is not interrupted
either — there is no call to hand back, and a synthetic one would be worse than
the miss.

The sandbox boundary should remain useful outside the desktop product. A local
process sandbox is the first execution adapter; self-hosted and managed profiles
may later supply stronger isolation behind the same contract.

Which adapter a child gets is decided once per server process, not per spawn.
The default is the in-process worker described above, whose isolation is the
narrow tool surface rather than a runtime boundary. Setting
`OPENWAVE_CONTAINER_EXECUTION_ENABLED=true` routes newly admitted children to a
local container instead, when a container runtime is detected; an absent
runtime reports the capability as unavailable and admission falls back to the
in-process worker rather than failing. A container-located run is attached-only
and proxies its model inference back through the host, so no model credential
enters the container. See [Sandbox providers](sandbox-providers.md).

### Delegation and consent

Both orchestration tools declare the `Sensitive` approval class. A child's own
calls never pass back through the parent chat's approval gate — the sandbox
worker advances them under its own lease — so the delegation is the only place
in the chat where the reader could speak about what the child does. Classing it
below the authority it hands out would state the opposite.

Declaring the class is not the same as gating on it. The gate parks a *pending
server tool call* on a durable approval receipt, and a spawn has no such
record: its tool call is written already completed, inside the same transaction
that admits the child. So the class today decides advertisement — a plan turn
never sees the pair — and not admission. The consequence worth naming: in a
chat where a foreground `web_search` would stop and ask, the same egress
performed by a delegated child does not. Closing that requires a durable
pending-spawn checkpoint the reader can answer, which is its own change
(issue #1477), not a flag.

## Bounded scheduling

Concurrency control has three independent purposes: protecting the machine,
keeping one chat from monopolizing it, and preventing an agent from creating an
unbounded queue. The scheduler therefore applies configurable limits to:

- background agents running across the installation;
- running children per chat;
- four unsettled children per foreground turn (spawn admission and
  `wait_for_agents` share this ceiling), including delivered results that have
  not yet been consumed;
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
run and the exact lease segment that submitted it. The ordinary way a run ends is
a submission: the run calls `done` with the filenames it wrote under `output/`
and a short summary, and the receipt records the outputs the scan already
published under those names alongside the summary. Naming a file the run never
wrote fails the completion like any other malformed one, bounded by the run's own
attempt budget; a run that genuinely produced nothing submits nothing. Final text
remains the receipt for a model that simply stops without submitting. The only
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
server-side. A sandbox snapshot also carries its opaque OpenWave spawn-call id
so the transcript can attach it to the exact visible delegation row; this is a
renderer correlation key, not a provider call identity or scheduler control.

When an agent has a live, supported tool checkpoint, the snapshot may also
contain a small `activity` object. Its values are a deliberately admitted,
fixed display vocabulary—for example, sandbox `exec` and `web_search`, or foreground
`list_connected_folders`, `list_folder`, and `read_connected_file`, each with
`waiting` or `running` status. It is not a tool trace: queries, tool arguments,
results, folder/root identities, relative paths, filenames, host paths, grants,
provider identifiers, executor leases, and raw failures remain server-side.
New tools are invisible to the renderer until they receive their own safe
activity projection.

A snapshot of a background run may also carry how far its own task plan has
got: how many steps are completed, out of how many, and the one step marked
`in_progress`. That last field is the exception to the fixed-vocabulary rule
above, and it is the same exception the activity headlines already make — the
text is the run's own, bounded and single-line before storage accepts it. The
whole ordered list is a separate read,
`GET /chats/{chat_id}/agent-runs/{run_id}/task-plan`, bound to the exact chat
and run like the two below it; a run that made no plan answers `null`.

`GET /chats/{chat_id}/agent-runs/{run_id}/activity` rebuilds the background
run's ordered history from those durable checkpoints. Each item keeps the fixed
kind, coarse outcome, and timestamp, and may add one typed headline: the bounded
command vector, recorded exit code, and captured output tail of a settled `exec`
step; the bounded public-web query; or the leaf name of the one delegated file.
Command, argument, and query strings are model-authored: the display clamp
bounds and de-spoofs them, but they may repeat anything the child already saw
and are outside the host-field non-disclosure guarantee.

A settled `exec` step's output tail is the one deliberate exception to the rule
that stored results stay server-side. A sandbox command runs in a private,
initially-empty workspace containing only what the run itself staged, so what it
printed is the command's own text rather than host- or user-derived content, and
without it a failed background command is unreadable. The tail is bounded to
2,000 characters, carried only for terminal steps, and taken from the whole
receipt rather than from the section after a `stdout:` marker, so a command
cannot choose what the card shows by printing that marker itself; control
characters other than newlines and tabs are dropped. It is bounded that
tightly because the endpoint returns a run's entire history in one response and
the panel re-fetches on update. The exception does not extend to web-search
results or delegated-file contents, which carry material the run was handed and
are still never projected.

The server does not directly copy any other stored result, nor full broker
paths, opaque root identities, provider identities, executor leases, or raw
diagnostics. The other host-derived details are the typed numeric exit code and
delegated leaf name. The detail key is optional, so a call without a derivable
headline retains the original three-field shape; no separate activity-history
payload or database migration is involved.

The desktop consumes that projection in two places. The transcript renders one
status row per spawned child beside the delegation it came from, correlated by
the snapshot's spawn-call id, and a background-agent panel renders the same
runs with their activity history and a Stop control for cancellable sandbox
states. Both keep the posture the projection assumes: every request is bound to
the exact chat and run, a pending or retryable error state is held until
polling confirms the durable transition, and no worker lease or direct
scheduler control ever crosses the boundary.

Those surfaces answer *what state* a child is in and *which step* it is on.
Neither answers what the child is actually doing, which until it submits a
result is the only thing a reader wants to know.
`GET /chats/{chat_id}/agent-runs/{run_id}/progress` is that stream: the bounded
lines the run itself published, each stamped with a monotonic per-run sequence.
A reader polls with the cursor it last saw and receives only what has arrived
since, which is what makes watching a long child cheap. A missing, wrong-chat,
or foreground run answers `404`, exactly as the activity history does.

Two producers write it, and neither is trusted to write it correctly for the
stream to stay usable. A container-located run's progress comes from the
sandbox protocol's own sequenced event stream; an in-process run's comes from
the text its model produced before it checkpointed on a tool. Each line carries
its producer's identity for that line — an event sequence, or the durable
checkpoint the narration belongs to — so a reattached container redelivering a
batch, or a worker retrying an ambiguous commit, leaves one line rather than
two.

The stream is deliberately outside the correctness contract. No transition
reads it; the append takes no lease and commits separately from the transition
that produced the line; a failure to publish is reported and dropped rather
than allowed to disturb a checkpoint that already committed. Retention bounds
it, so a run that narrates for an hour costs a bounded number of rows and an
observer that stops reading may find the oldest lines gone. What it does
guarantee is order, and that a line already delivered is never delivered twice.
The text is the run's own prose — the same class the terminal result already
carries. It may repeat information the run already saw, so the boundary promise
is about provenance rather than content: the progress path does not directly
copy stored tool arguments/results or host-owned identities into the stream.

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
12. Add fenced read-only tools to the foreground turn for roots its chat already
    attached. *(Shipped.)*
13. Add desktop surfaces for queued, running, waiting, failed, and completed
   background work, including exact sandbox stop controls. *(Shipped.)*
14. Persist the non-blocking spawn checkpoint and ordered wait receipts.
    *(Shipped.)*
15. Activate non-blocking spawn and ordered multi-agent waits together.
    *(Shipped.)*
16. Add the desktop-only, argument-free read of one exact file immutably
    delegated at spawn, with native claim/revalidation, broker authorization,
    revocation fencing, and crash-safe no-replay recovery. *(Shipped.)*
17. Add richer context lifecycle, parallel-safe tool groups, and further
    orchestration only after these recovery boundaries are proven.
