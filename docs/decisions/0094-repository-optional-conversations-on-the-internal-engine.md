# 94. Repository-optional conversations on the internal engine

- Status: Accepted
- Date: 2026-09-09
- Owners: server, code mode
- Related: [0030](0030-code-mode-separate-surface.md), [0048](0048-one-interaction-model.md), [0090](0090-a-session-acts-as-one-forge-identity.md); `docs/slack-sessions.md`; `docs/code-mode.md`; #3185, #3191, #3192
- Supersedes: none

## Context

A Slack conversation must start without selecting a repository; Slack itself defaults
to that behavior. The internal engine can answer, ask, triage, discover accessible
repositories, and create or drive children in several independent repository workspaces
under the same parent conversation. Choosing a single installation repository, or only
preparing one owner checkout, does not satisfy this. Decision 30s "context selects
behavior" sentence is amended here: no repository no longer selects the old chat-only
surface, and decision 48 step 5s repository-less session path is the supported floor.

## Decision

`POST /external/code/sessions` accepts neither `repo_id` nor `repository` and creates a
workspace-less session on the machines internal engine, bound to the same external grant.
An explicit `repo_id` or `repository` preserves the existing repository-backed path,
including sandbox placement when configured.

The internal engine exposes native self-drive tools `code_repos`, `code_session_create`,
`code_run_turn`, `code_wait`, and `code_sessions`. One conversation chooses repositories,
starts child sessions in new workspaces, drives them, and reads their results. Tool names
and argument shapes reuse the `agent-mcp` vocabulary where the surfaces overlap.

Children inherit the parents owner, grant, permission mode, and forge identity. Child
creation is a Sensitive action; reads and waits are ReadOnly. A revoked grant fails
discovery, creation, and child reads closed. Workspace grants require per-channel
repository confirmation before cloning, and a clone is owner-scoped and grant-bound.
A child create uses a stable `request_key`, and a retry after `repository_preparing`
observes the same owner-scoped clone job. If a crash commits the child binding before its
context row, the next retry with the same key repairs the context and reuses the child
instead of creating a duplicate.

`code_wait` is a bounded 20-second polling read, not a durable child wait/resume park.
Sandbox children, tree-aware spend budgets, the session tree UI, and agent-MCP mounting
with a session-scoped token are declared follow-up, not completeness.

## Alternatives considered

- Keep a repository requirement for version one. Rejected: the product and the Slack
  defaults require a no-repository start, and the internal engine already hosts
  workspace-less sessions.
- Start children only from repositories already registered on the machine. Rejected: triage
  and fan-out need the conversations forge-accessible repository discovery.
- Let children diverge from the parents permission mode, execution location, or forge
  identity. Rejected: a child must never broaden posture or identity, and a person grant
  must not silently fall back to the bot.

## Consequences

- The "scratch stage" in `docs/slack-sessions.md` is retired and the repository-optional
  machine contract is documented there and in `docs/code-mode.md`.
- A conversation with no workspace is still a code session. Its web link is
  `/c/{session_id}`; a workspace child links through `/code/w/{workspace_id}`.
- Known limitations are recorded rather than claimed: durable wait/resume, sandbox
  children, budgets, tree UI, agent-MCP mounting, and an atomic context write (the
  crash is repaired on retry, not prevented in one transaction).

## Validation

- `cargo test -p tidebreak-server-core --lib code::self_drive::tests -- --nocapture`:
  three tests cover two-repository fan-out, wait, and retry; a revoked grant; and
  crash-before-context repair plus stranger-parent denial.
- `cargo test -p tidebreak-core --lib db::migration::tests::a_fresh_database_records_the_whole_chain`
  and the stepwise-upgrade test cover the appended `code_session_context` migration.
- The API-level no-repository regression could not run in this sandbox because loopback
  HTTP is blocked (the test client cannot reach its own router); the server crates compile.
