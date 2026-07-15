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
5. The child submits a terminal result or a durable failure.
6. Completion is delivered to the parent's durable inbox and wakes an explicit
   parent wait, if one exists.

The foreground agent may continue or finish its current turn after spawning a
child. If it needs the result before proceeding, it parks on a durable wait and
releases its worker instead of polling in memory.

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

## Reliability contract

The agent hierarchy preserves the runtime's existing rules:

1. Accepted work is already durable.
2. One exact lease owner advances a run at a time.
3. Stale workers cannot publish messages, effects, results, or events.
4. Every externally visible tool effect has a stable call identity and terminal
   receipt before it becomes safely retryable.
5. Waits release workers and resume from committed checkpoints.
6. Steering and cancellation are resolved at explicit boundaries.
7. A final result, terminal run state, parent delivery, and terminal event are
   committed atomically.
8. Clients recover from a durable snapshot plus ordered event replay.

Until a tool satisfies the side-effect receipt contract, OpenWave continues to
fail conservatively after an ambiguous execution rather than replay it.

## Delivery sequence

The implementation is intentionally incremental:

1. Add the durable `AgentRun` hierarchy and depth-one constraints.
2. Generalize client execution into the shared continuation model.
3. Persist model/tool step boundaries and side-effect receipts.
4. Add the bounded sandbox scheduler and sandbox lifecycle.
5. Add idempotent spawn, wait, cancel, inbox, and result-submission operations.
6. Route sandbox folder access through the host broker.
7. Add desktop surfaces for queued, running, waiting, failed, and completed
   background work.
8. Add richer context lifecycle, parallel-safe tool groups, and further
   orchestration only after these recovery boundaries are proven.
