# 8. Settled folder positions

- Status: Accepted
- Date: 2026-08-11
- Owners: host access / broker
- Related: [`docs/host-access.md`](../host-access.md),
  [`0002-pre-v1-schema-and-persisted-format-mutability.md`](0002-pre-v1-schema-and-persisted-format-mutability.md)

## Context

The host broker is deny-by-default: an agent reaches a folder only through a
grant the user gave. Grants are keyed on a grant subject — a project, or a
standalone conversation. Attachments are keyed on the conversation. For a chat
inside a project those are different entities, and every bug this record
addresses came from code that asked one of them a question only the other could
answer.

Revoking a grant deletes the row. Nothing else records that the user was ever
asked, so "narrowed to nothing" and "never granted" look identical from the
state file. Arrival paths — registering a folder, attaching one to a chat —
minted default grants when they found none, which meant they minted after a
revocation as readily as on a first pick. Observed consequences:

- A sibling chat in the same project attaching a folder restored every
  capability its neighbour had just revoked.
- A chat that disconnected a folder it had emptied got the full set back on
  reconnect.
- Detach was gated on holding read, so a user who revoked read on a connected
  folder could neither use the folder nor remove it. That gate was also, by
  accident, the only thing holding up the detach-then-reattach case above:
  fixing the lockout reopened the minting hole, which is why both land together.

The forces: the user's answer has to survive a revocation, and it has to be
visible to every arrival path regardless of which key that path happens to hold.

## Decision

Access decisions are recorded per **position** — a `(grant subject, folder)`
pair — independently of whether any grant currently exists for it.

- A position is settled the first time the user answers for it: registering the
  folder, or an arrival that mints defaults into it.
- **No arrival mints into a settled position.** Registration, attachment, and
  re-picking a folder already known are all arrivals. They may hand a chat the
  folder; they never widen what it allows.
- Widening is only ever the host's own permission dialog, answered explicitly.
- Narrowing is unrestricted, and narrowing to nothing is a legitimate resting
  state. A folder that allows nothing stays attached and stays listed, so the
  surfaces that list it can offer both the way out (disconnect) and the way back
  (ask for read again).
- **Disconnecting exercises no access to the folder** and is never gated on
  holding any capability.
- A position is forgotten when the folder's approval is revoked or the subject
  is purged. At that point the position no longer exists and picking the folder
  again is a genuine first arrival.

Deliberately excluded: any attempt to infer the answer from the grant table.
The grant table cannot answer it, and this record exists because that inference
was tried and was wrong three separate ways.

### The persisted obligation this introduces

The record is durable state, so the state file gains it and gains a version.
Two rules bind every future change to that file:

1. **The accepted-version set widens; it never shifts.** Adding a version must
   not drop one an install has on disk. A refused state file is a broker that
   does not start, which presents to the user as every folder gone — for
   everyone, on upgrade. This is the general rule for this file, not a one-off
   for version 5.
2. **Older files are reconstructed, not defaulted.** An absent record must not
   read as "nothing settled", because that is exactly the state that re-mints.
   Versions 2 through 4 are reconstructed from the evidence they do carry: a
   surviving grant over the folder; a surviving attachment, which settles only
   its own conversation's subject; and the folder's registration owner, which is
   what recovers a project chat. The reconstruction accepts exactly the evidence
   the loaded-state validation accepts, so a file consistent under the old rules
   stays loadable under the new ones — including the emptied folder this record
   exists to serve, which the old validation rules refused outright.

## Alternatives Considered

**Keep the grant row as a tombstone with no capabilities, instead of deleting
it.** This is the alternative a reviewer reaches for first, and it is genuinely
attractive: no new structure, no new version, and revocation stops destroying
information at the point where the information is lost. It was rejected on
blast radius. `state.grants` is read by authorization, by the listing surfaces,
by loaded-state validation, and by the product projection the desktop renders;
a tombstone changes the meaning of "there is a grant" for every one of them at
once. Each site must then be audited to skip empty rows, and every site missed
fails open in a different direction — a listing that renders a tombstone shows
the user access they do not have, and an authorization path that counts one is a
capability leak. A separate structure that only the arrival paths consult cannot
fail that way: code that does not read it behaves exactly as it did before.
The tombstone also has to answer what an empty row means when the folder's
approval is revoked outright, which is the case where the position genuinely
should be forgotten.

**Put the grant subject on the attachment and compare it on arrival.** Fixes the
sibling-project case, and is cheaper. Rejected because it cannot fix the
detach-then-reconnect case at all: after a detach there is no attachment left to
consult, so the reconnect is indistinguishable from a first pick. Half the bug,
in a shape that reads like the whole fix.

**Do nothing and treat the mint-on-arrival behavior as intended.** Rejected:
it makes revocation advisory. Any chat in the same project, and the same chat
after a disconnect, silently undoes it.

## Consequences

- The state file is at version 5, and versions 2 through 4 carry a
  reconstruction path that must keep working for as long as those files exist.
  Future versions inherit both rules above.
- Removing the detach gate means a detach request is honored for a folder the
  requester holds nothing over. That is intended — detaching reads no bytes —
  but it does mean detach is no longer incidentally covered by capability
  checks, and its own tests are the only thing pinning it.
- The settled set is per subject and folder, so it grows with folders the user
  has answered for and shrinks only on revocation or subject purge. It is not a
  cache and must not be pruned on size.
- A user who narrows a folder to nothing now stays narrowed until they ask for
  access back in the dialog. Any future surface that offers a folder to a chat
  has to route widening through that dialog rather than through its own
  convenience path.

Revisit if grants and attachments are ever re-keyed onto a single entity. Most
of the pressure this record absorbs comes from the two keys, and a design where
an attachment and a grant name the same thing could carry the answer on the
grant itself.

## Validation

- An arrival into an emptied position mints nothing: revoke every root-scoped
  grant, then attach from the same chat, from a sibling chat in the same
  project, and after a detach — each must yield the folder and no capabilities.
  A wrong implementation that only compares the conversation key passes the
  first and fails the sibling case; one that consults the attachment passes both
  and fails the detach case.
- The emptied position survives a restart: the same sequence with the broker
  dropped and reopened between the revoke and the attach. This is the assertion
  that fails if the record is not durable, and it is also what caught the old
  validation rules refusing to load such a file at all.
- A version-4 file loads. This is the check a plausible wrong implementation
  fails on its first run and no unit test of the new behavior would catch: the
  version gate was written as a shift, and every existing install would have
  lost its folders on upgrade.
- A version-4 file with an emptied folder loads *and* comes back settled —
  loading is not enough, because a file that loads with an empty record re-mints
  on the next attach.
- A folder that allows nothing can still be detached.
