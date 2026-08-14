# 28. Apps Have Immutable Principal Ownership

- Status: Proposed
- Date: 2026-08-14
- Owners: app runtime and storage
- Related: 0006-self-host-deployment-plane-authorization.md, principal-scoped storage
- Supersedes: none

## Context

Local apps were introduced as profile-scoped records. That was exact for the
single-user desktop profile, but a shared self-host database now serves named
principals. An app id addresses revisions, grants, invocation, view sessions,
and gateway publication; filtering only the library route would leave the same
record reachable through those secondary surfaces.

Creation happens inside a recorded tool call. The durable conversation already
has an owner, while request routes carry the authenticated principal's
`OwnerId`. Both are trusted server-side sources; renderer and tool arguments
are not.

## Decision

The app row is the ownership root. It stores one non-null `owner` value stamped
at creation and never updated. Existing pre-v1 rows use the `local` default.
Revisions, grants, and gateway drafts inherit authority through their foreign
key to the app rather than duplicating an independently mutable owner column.

Every request-facing app operation uses owner-scoped store methods. A row owned
by another principal is indistinguishable from a missing row. Mutations lock
and test the app row for the same owner in the transaction that appends a
revision, deletes or restores the app, replaces or revokes a grant, or replaces
a gateway draft. Tool-created apps derive the owner from the recorded call's
conversation in the creation transaction; later revisions require the
authoring conversation and target app to have the same owner.

Gateway registration carries the already-authenticated owner through its
background and network lifecycle. Single-use frame redemption remains
capability-based, but only an owner-authorized request may mint the token.

## Alternatives Considered

- Keep apps deployment-wide and authorize only the library. Rejected because
  guessed app ids would still reach detail, grants, invocation, frames, or
  gateway publication.
- Copy `owner` onto every app child table. Rejected because duplicated policy
  can drift; the app foreign key is the one immutable authority root.
- Infer ownership from the latest revision's `chat_id`. Rejected because that
  field is nullable provenance, may dangle after chat deletion, and would make
  ownership change when a different conversation publishes a revision.
- Trust an owner supplied by `create_app` arguments. Rejected because model and
  renderer input is not an authentication boundary.

## Consequences

The schema baseline changes and the disposable desktop schema epoch advances.
All app-specific store APIs gain scoped variants, and gateway draft helpers
carry an `OwnerId`. Cross-owner attempts intentionally read as absence, which
prevents ids from becoming an ownership oracle.

This decision should be revisited only if apps become explicitly shareable or
transferable. That would require a separate membership/delegation model and an
audited transfer operation; changing the owner column in place is not such a
model.

## Validation

Database tests create two principals and prove that the second cannot list,
read, revise, delete, grant, revoke, or read/write gateway draft state for the
first principal's app. Route tests prove that an authenticated second principal
receives the same not-found/refusal behavior as for an unknown app, while the
owner retains normal access.
