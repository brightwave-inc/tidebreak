# 96. Slack channels share the instance GitHub App repository access

- Status: Proposed
- Date: 2026-09-10
- Owners: thet
- Related: [0090](0090-a-session-acts-as-one-forge-identity.md); [0092](0092-workspace-grants.md); [0093](0093-a-session-can-have-several-conversation-bindings.md); [0094](0094-repository-optional-conversations-on-the-internal-engine.md)
- Supersedes: the per-channel repository approval requirements in decisions 92, 93, and 94

## Context

The GitHub App installation already determines which repositories the shared
Tidebreak identity can use. Requiring an administrator to type those repositories
again in Channels creates a second permission system. Approval in one channel
also leaves another channel blocked, even though both use the same instance and
GitHub App.

## Decision

The configured GitHub App installation defines repository access for a Tidebreak
instance. Every Slack channel connected to that instance inherits that access.
No channel repository allowlist, approval prompt, or repository-entry form is
required. A channel default selects work; it does not grant or restrict access.
A repository-less conversation may discover and choose several repositories.

New workspace-grant tasks, child creation, and attachment to another channel
check the live configured forge for the exact GitHub repository. Registered
checkouts receive the same check, so a stored checkout or historical channel
approval cannot replace the GitHub App's authority. Discovery results do not
become a separate allowlist. Each Git operation continues to borrow its own
repository-scoped credential.

Workspace connection approval, grant revocation, session ownership, membership,
and conversation bindings keep their existing meaning. Personal sessions keep
their personal GitHub identity. This decision does not change who can join a
channel or read a conversation. Administrators choose the shared repository
access through the GitHub App installation before connecting the Slack workspace.

Historical repository confirmation rows and their administrator-only endpoints
remain for compatibility with older clients. They do not authorize or block work
on this version. The Channels screen no longer presents them as permissions.

## Alternatives considered

- Approve once per channel in Slack instead of Tidebreak. Rejected because it
  keeps the duplicate permission model and does not share access across channels.
- Add a repository allowlist per workspace or machine. Rejected because it still
  duplicates the GitHub App installation's controls.
- Remove the channel gate without checking cached repositories. Rejected because
  a stored checkout could admit new work after the configured app loses access.

## Consequences and validation

An administrator manages repository access once in GitHub. Every connected
channel can use the permitted repositories without a Tidebreak approval.
Tests cover two channels using three repositories with no confirmation rows,
child sessions choosing several repositories, attachment to another channel,
and forge refusals despite cached checkouts or historical approvals.

Revisit this only if the product explicitly supports separately configured
GitHub identities for different Tidebreak instances. A channel remains a routing
and conversation context, not a repository permission boundary.
