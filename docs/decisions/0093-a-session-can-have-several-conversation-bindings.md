# 93. A session can have several conversation bindings

- Status: Accepted
- Date: 2026-09-08
- Owners: thet
- Related: [0092](0092-workspace-grants.md); `docs/slack-sessions.md`; #3198
- Supersedes: none

## Context

A session outlives the thread that starts it. The binding table already permits several conversations to reach one session, but the adapter API only creates a session with its first binding. Moving a conversation must preserve its execution identity, access, and journal.

## Decision

An adapter attaches a conversation through `POST /external/code/sessions/{id}/bindings`. The grant must already hold that session. The destination key stays within the grant's channel family. Attaching the same key to the same session is idempotent. A key held by another session or grant returns the same not-found response as an inaccessible session. An ended session refuses attachment.

The session row lock serializes attachment with lifecycle updates. The existing unique conversation key resolves concurrent attachments. Attaching a workspace-grant session also requires an administrator's repository confirmation for the destination channel. The adapter remains responsible for Slack workspace and membership checks; an opaque key does not confer access.

`GET` on the bindings route lists the grant's bindings. Session snapshots expose all origins in creation order and keep `external_origin` as the first origin for existing clients. Each thread reads the same session journal with its own cursor. Attaching a binding neither copies the journal nor changes ownership or contributors.

## Alternatives considered

Copying the session would split its history and delivery state. Moving its sole binding would make the old thread stop receiving results. Crossing grants would silently change the authority behind a conversation. Each conflicts with the requirement that several threads continue the same session.

## Consequences

An adapter stores rendering state per binding. Access changes across private and public channels remain explicit; attaching a binding does not grant a web viewer access. Revisit this boundary if a session must move across Slack workspaces or execution identities.

## Validation

HTTP tests cover idempotent attachment, concurrent attachment, foreign grants, destination conflicts, ended sessions, destination repository confirmation, and binding reads. Two event readers receive the same session journal, and both snapshots list the attached conversations.
