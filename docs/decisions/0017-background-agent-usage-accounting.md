# 17. Background Agent Runs Own Cumulative Usage Accounting

- Status: Proposed
- Date: 2026-08-14
- Owners: core storage, sandbox workers, agent-run API
- Related: [`docs/agent-runs.md`](../agent-runs.md),
  [`0015-compaction-rides-the-conversation-cache.md`](0015-compaction-rides-the-conversation-cache.md)
- Supersedes: none

## Context

Foreground turns persist model-step and token usage counters. Background
sandbox agents are separate model executions with their own task prompt,
retries, checkpoints, and cost, but their durable `agent_run` and immutable
result contain no usage. The in-process worker currently discards provider
usage events, container results expose no counters, and `agent-run show` cannot
report whether a child received an unexpectedly large prompt or used cache.

Usage attached only to the final result would also be incomplete: a failed
attempt can consume tokens before a retry succeeds, and a check-in can pause a
run before its final receipt. Accounting must survive every lifecycle state.

## Decision

**Each background `agent_run` durably owns cumulative model-step and usage
counters across all of its attempts.**

1. The run stores `model_steps` and the four disjoint usage counters used by
   foreground turns: uncached input, cache-read input, cache-creation input,
   and output.
2. Every completed provider model step atomically adds its reported usage and
   increments `model_steps` before the worker advances, checks in, retries, or
   submits a result. Attempts accumulate rather than replace one another
   because they all consumed real provider work.
3. A retried persistence operation is idempotent by the worker's existing
   durable step/checkpoint identity. Counters must never double-apply after an
   ambiguous commit or worker restart.
4. Terminal results snapshot the cumulative counters for convenient immutable
   audit, while the run remains authoritative for live and non-final states.
5. `agent-run show` exposes the counters for background runs. Foreground run
   identities are not presented as live background jobs: list/detail must agree
   on which objects are addressable.
6. Providers that do not report cache leave cache counters at zero. The API
   does not infer a cache miss from absent provider telemetry.

## Alternatives Considered

- **Store usage only in logs.** Rejected because logs are neither durable API
  state nor reliably attributable after retries.
- **Store usage only on the terminal result.** Rejected because failed attempts
  and check-in states would disappear from accounting.
- **Count only the successful attempt.** Rejected because it understates cost
  and hides retry storms.
- **Represent each child model step as a foreground `turn_run`.** Rejected for
  now because child runs have a different lifecycle and no conversation
  message identity; forcing them into the foreground table would blur both
  contracts.
- **Do nothing.** Leaves delegated cost, cache, and context regressions
  unauditable.

## Consequences

The agent-run schema and wire snapshot gain additive counters, and both
in-process and container workers must report usage through one idempotent store
operation. Existing rows migrate with zeros, meaning historical absence remains
indistinguishable from a provider that reported none. Run detail becomes useful
for cost and context audits, while foreground conversational state remains in
the turn APIs rather than masquerading as an active background run.

Revisit if provider billing exports become the authoritative accounting source,
or if child execution is unified with foreground turns under one lifecycle
table.

## Validation

- A one-step child persists exactly its provider usage and `model_steps = 1`.
- A multi-step child accumulates all disjoint fields without rewriting cache
  categories.
- A failed attempt followed by success includes both attempts exactly once.
- Replaying an ambiguous checkpoint does not double counters.
- Check-in, cancellation, and failure retain counters even without final text.
- `agent-run show` reports the same totals as storage for a completed child.
- Foreground identities are either omitted from `agent-run list` or have a
  working detail representation; list never returns an unshowable `active` id.
