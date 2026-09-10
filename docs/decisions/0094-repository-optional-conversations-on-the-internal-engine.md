# 94. Repository-optional conversations on the internal engine

- Status: Accepted; per-channel repository approval superseded by [decision 96](0096-slack-channels-share-the-instance-github-app-repository-access.md)
- Date: 2026-09-09
- Owners: server, code mode
- Related: [0030](0030-code-mode-separate-surface.md), [0048](0048-one-interaction-model.md), [0090](0090-a-session-acts-as-one-forge-identity.md); `docs/slack-sessions.md`; `docs/code-mode.md`; #3185, #3191, #3192
- Supersedes: none

## Context

A Slack conversation must start without selecting a repository. The internal engine
can answer questions, triage work, discover accessible repositories, and drive child
sessions in several repository workspaces. This extends decision 30's context-based
behavior and uses decision 48's session path without a workspace.

## Decision

`POST /external/code/sessions` accepts a request without `repo_id` or `repository`.
It creates an internal-engine session on the machine and binds the conversation to
its external grant. An explicit selector keeps the repository-backed path, including
sandbox placement when configured. A retry resolves its existing binding before
inspecting repository selectors. Additional channel bindings preserve the original
session channel.

Before creating a conversation without a repository, the server resolves and freezes
the chat model. On a Gateway host, the original grant's delegation supplies the catalog
and inference credential. Browser credentials cannot replace that delegation. A missing
model or revoked grant refuses admission. Each model request checks the grant again,
and hosted model routes validate the frozen selection before HTTP dispatch.

The internal engine exposes `code_repos`, `code_session_create`, `code_run_turn`,
`code_wait`, and `code_sessions`. A conversation can select repositories, create child
sessions in independent workspaces, send follow-ups, and read their results. Some names
also exist in `agent-mcp`, but their schemas are not interchangeable. Schema alignment
and a session-scoped MCP capability remain in #3192.

Children inherit the parent's owner, grant, and forge identity. Machine children retain
the parent's permission mode. Children placed in a configured sandbox use Allow under
the sandbox's confinement policy. Child creation is Sensitive; reads and waits are
ReadOnly. A revoked grant refuses discovery, creation, and child reads. Workspace grants
require an approved channel repository scope before cloning. An administrator can approve
several repositories together in Settings > Channels before any child starts. A child
inside that scope needs no further channel confirmation. Requests outside the scope
remain pending independently until the administrator adds them.

Child creation uses a stable `request_key`. A retry after `repository_preparing`
observes the same owner's clone job. If a crash commits the child binding before its
context row, the next retry repairs the context and reuses the child. Snapshots return
the latest top-level answer, report truncation, and include current failure or fence
information.

`code_wait` polls for at most 20 seconds. Durable child wait/resume (#3191), the broader
sandbox-child tool contract (#3193), tree budgets (#3194), and tree UI (#3195) remain.

## Alternatives considered

- Require a repository at conversation start. Rejected because questions and triage
  often precede repository selection.
- Limit children to registered repositories. Rejected because the conversation needs
  to discover repositories accessible to its forge identity and prepare them on demand.
- Let children switch forge identity or broaden machine permissions. Rejected because
  delegated work must retain the parent's authority. A person connection cannot
  silently become a bot connection.

## Consequences

A conversation without a workspace remains a code session at `/c/{session_id}`.
A workspace child links through `/code/w/{workspace_id}`. Work across repositories uses
several workspaces; each workspace still belongs to one repository. Binding and context
writes remain separate, with recovery through the same request key.

## Validation

API tests exercise repository-less inference through the configured Gateway resolver,
frozen model selection, original-grant revocation, empty catalogs, binding retries,
and attached channels. Child-session tests cover two repositories, permission modes,
request-key recovery, foreign-parent refusal, final output, and failures. Migration
tests cover fresh databases and stepwise upgrades for `code_session_context`.
