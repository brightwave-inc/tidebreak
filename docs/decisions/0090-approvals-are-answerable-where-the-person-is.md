# 90. Approvals are answerable where the person is

- Status: Accepted
- Date: 2026-09-08
- Owners: thet
- Related: [0039](0039-allow-is-a-first-class-code-permission-mode.md); [0086](0086-session-access-is-separate-from-ownership.md); [0088](0088-a-slack-session-runs-where-the-deployment-can-run-it.md); `docs/slack-sessions.md`; tidebreak #3178 (track C), #3186
- Supersedes: the "do not approve engine actions from Slack" paragraphs of `docs/slack-sessions.md`

## Context

A sandbox session is Allow and never asks. Decision 88 put machine sessions
on the deployment's default mode, so Default and Plan park on approvals,
questions, and plans. Slack is where the person is. The desktop already
settles those cards through one path; the channel could not.

## Decision

You answer a parked card from the surface you are on. The external event
stream carries the card's facts for a machine session — tool name, class,
consent kind, grant ladder, a size-capped preview, and a flag when that
preview was cut — and `ApprovalResolved` names who decided. A sandbox
session still carries none of these events.

You settle through the same path the desktop uses, on
`POST /external/code/sessions/{id}/approvals/{call}/decision` (and the
question and plan aliases), as a contributor. The actor is the grant's
external identity, or the optional body `actor` a workspace grant will
send. A card already settled elsewhere answers `already_settled` with who
decided. Approve-and-remember is offered only for the internal engine.

## Alternatives Considered

**Keep Slack read-only for consent.** Rejected: a machine session in Ask
would park with no one in the thread able to unpark it.

**A second settlement path for the adapter.** Rejected: two writers on one
row is how a card gets two answers.

## Consequences

The adapter can render a card and post a decision without loading the
approval row. A truncated preview is a prompt to open the web link, not a
reason to send the rest of the payload over the channel.

Revisit if a channel needs the full preview in-thread, or if a sandbox
placement ever parks on consent.

## Validation

- A Default-mode machine session parks on a shell command; a contributor
  approves; the journal names them; a second decision answers
  `already_settled`.
- The stream's preview is size-capped and flags truncation.
- A standing grant is refused for an engine that does not declare it.
- A sandbox session emits none of these events.
