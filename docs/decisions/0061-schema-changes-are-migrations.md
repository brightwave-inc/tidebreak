# 61. Schema Changes Are Migrations, Not Database Resets

- Status: Proposed
- Date: 2026-08-21
- Owners: core
- Related: [`crates/tidebreak-core/src/db/migration.rs`](../../crates/tidebreak-core/src/db/migration.rs)
  (the chain and its frozen baseline),
  [`crates/tidebreak-server/src/desktop_schema.rs`](../../crates/tidebreak-server/src/desktop_schema.rs)
  (`LAST_RESET_EPOCH` and the convergence it drives),
  [1.0.0 checklist](../releases.md#preparing-and-shipping-100) item 2
- Supersedes: [decision 2](0002-pre-v1-schema-and-persisted-format-mutability.md)

## Context

Decision 2 made a pre-v1 schema change a baseline edit plus a
`DESKTOP_SCHEMA_EPOCH` bump, and the bump deleted the local database. It said
the window closes at `1.0.0` and put the inversion on the 1.0 checklist.

**What the window actually cost.** Eleven epoch bumps between 2026-08-12 and
2026-08-21, eight of them after the baseline squash on 2026-08-14 — roughly one
wipe a day. Eight are code-mode work. Each one deletes every local chat, every
code session, and every code turn on every contributor's machine, and strands
the worktrees those sessions were driving.

**The bump is a judgment call, and it has been missed twice.** Auditing every
baseline-touching commit since the 2026-08-14 squash against the epoch in the
same commit: #2289 added `code_session.kind` and #2453 added
`code_session.reasoning_effort` and its CHECK, and neither moved the integer.
Both regenerated the fixture instead, which is what a green run looks like
either way — the fixture makes a baseline edit visible, it cannot decide
whether the epoch owes a bump. Both were repaired by chance, because the next
change that *did* bump rebuilt the profile from the current baseline. The
window between an un-bumped edit and the next bump is a contributor booting
into a schema that is missing a column, and the only symptom is a query
failing.

Freezing the baseline removes the judgment call rather than reinforcing it.
After this, a fixture diff does not mean "decide whether to bump" — it means
"you edited the wrong file".

**The window was never open for self-host.** The epoch is a file beside SQLite;
PostgreSQL has no equivalent and no reset. `Baseline::up` handles an existing
self-host database by returning early, so it holds whatever the baseline said
on the day it first ran and gains nothing added since. Decision 2 described
local data as disposable and said nothing about this, because when it was
written self-host did not exist. The durable backend has been running without a
schema lifecycle the whole time.

**The four steps that justified waiting have landed.** Decision 48's remaining
steps were the argument for keeping the window open: they churn the schema, and
migrating each intermediate shape buys nothing. Steps 2, 3, and 4 are merged
(#2450, #2474, #2487). Step 5 (#2313) is the one change still big enough to be
worth a squash rather than a migration, and a squash is available at any time
while the product major is `0` — it is not a single shot that has to be saved.

## Decision

**A schema change is an appended migration.** `Baseline` is frozen. Anything
that changes the schema goes into `Migrator::migrations`, in order, where it
reaches a database that already exists as well as a fresh one.

**`LAST_RESET_EPOCH` is a pin, not a counter.** It names the baseline the chain
starts from, and it does not move for a schema change. It moves once more, at
the `1.0.0` squash, when the chain collapses back into a single baseline.

**Every existing profile converges or is reset exactly once, on the next
boot.** A profile at the pin holds precisely the baseline the chain starts
from, so its marker is re-stamped and its data kept. A profile below the pin
holds a baseline revision that was edited in place and never recorded, so no
migration can know what it contains; it takes one last reset. Neither path runs
twice.

Convergence rests on one thing being true at the moment this lands: the last
baseline edit on `main` is the one that set the pin. An un-bumped edit merged
after it would mean profiles at the pin are missing a column and would now keep
missing it, since the reset that used to repair that by accident is gone. That
is checkable before merging, and it is the last time it needs checking —
afterwards the baseline is frozen and `the_schema_baseline_is_pinned` refuses
the edit outright.

**Compatibility obligations for persisted formats resume.** Decision 2
suspended `#[serde(alias)]`, backfills, and data-preserving migrations because
the epoch deleted the rows. It no longer does. A journal payload change now
either stays readable from the shape already on disk, or migrates the rows that
hold it. The two journal fixtures' failure messages say so.

**Self-host keeps the gap it already has, and it is now bounded.** Nothing here
repairs a PostgreSQL database created before the pin — the tables it is missing
depend on the day it first ran, which nothing recorded. What changes is that
every schema change from here reaches it. Closing the pre-pin gap is tracked
separately (#2490); it needs a schema pin inside the database, which SQLite got
as a sidecar file and PostgreSQL never got at all.

Deliberately excluded: the product-major guard stays. A `1.0.0` binary still
refuses to open a local profile until the rest of the 1.0 checklist is done,
because this record settles how the schema changes, not what compatibility
surface the release commits to.

## Alternatives Considered

**Keep the epoch until #2313 lands, as #2428 originally said.** The order this
record follows instead. Rejected because the gate was never the squash — it was
churn, and the three steps that churned the schema hardest have landed. Waiting
for #2313 buys one tidier chain and costs another wipe a day until it merges,
on a step whose own timing depends on remote hosting (#2320–#2322).

**Flip at `1.0.0`, as decision 2 planned.** The status quo, and the cheapest
option to write down. Rejected for the self-host reason above: the durable
backend has no lifecycle at all today, and "we will build one at 1.0" leaves
every self-host deployment between now and then holding a schema nothing
maintains. It also front-loads the entire migration apparatus into the release
that can least afford surprises.

**Re-squash the baseline under a new name as part of this change.** Tidier: the
chain would start from today's schema rather than August 14's, and the two
owner migrations would disappear into it. Rejected because a rename makes
SeaORM re-run the baseline against every database that recorded the old name,
which is every database that exists. The rename has to happen where a reset is
already happening — at the 1.0 squash, or alongside #2313.

**Reset every profile once at the flip, rather than converging the ones at the
pin.** Simpler by one match arm, and it would make the transition uniform.
Rejected because it throws away exactly the data this record exists to keep,
on the one boot where keeping it is free: a profile at the pin already holds
the baseline the chain starts from. Spending a wipe to avoid an arm that a test
covers is the wrong trade.

## Consequences

Local data survives a schema change, and so a schema change costs more to
write. Adding a table means writing it once as a migration rather than editing
a baseline file, and a change to an existing column means saying how the rows
already in it get there.

The journal payload becomes a compatibility surface again. That is the sharpest
new constraint, and the one most likely to be forgotten: `list_events` collects
into `Result<Vec<_>>`, so a single unreadable row fails a whole chat's history
rather than one message. The fixtures make the change visible; they cannot make
it compatible.

The pre-pin self-host gap is unchanged and now stated. Anyone running a
PostgreSQL deployment created before this lands should recreate it; #2490 is
what removes the "should".

`reset_pre_v1_state` stays reachable, for profiles below the pin and for a
database with no marker at all. It stops being reachable when those profiles
are gone, which is not a date anyone can name — so the code keeps its tests
rather than being deleted on a guess.

Revisit this at the `1.0.0` squash, which is the next time the baseline is
supposed to move, or earlier if the pre-pin self-host gap turns out to need a
different shape than a pin row.

## Validation

`a_pre_v1_profile_at_the_pin_keeps_its_data_and_converges` boots a profile
carrying the last epoch-driven marker and asserts both halves: the chat is
still there, and the marker has been re-stamped so the next boot takes the
migrated path. This is the one path that runs once per profile and then never
again, which is why it is pinned by a test rather than by a manual check.

`a_profile_below_the_pin_takes_one_last_reset` covers the other side, and
`future_epoch_fails_closed_without_destroying_database` covers a marker this
binary does not understand — it refuses to open rather than resetting, so a
newer profile is never destroyed by an older binary.

`a_stepwise_upgrade_lands_on_the_fresh_schema` asserts that a database that
stopped at the baseline and later took the rest of the chain describes the same
schema as one built in a single pass. Without it the chain and the baseline can
disagree with nothing to notice, because each database is internally
consistent.

`the_schema_baseline_is_pinned` fails on any baseline edit, on both backends,
and its message says to append a migration instead of regenerating the fixture.

The case a plausible wrong implementation still passes: regenerating any of
these fixtures with its `UPDATE_*` variable is green. The fixtures make a change
visible; they cannot decide whether it is compatible. A green suite is not
evidence this record is being followed — the absence of a fixture diff in a
schema-touching PR is.
