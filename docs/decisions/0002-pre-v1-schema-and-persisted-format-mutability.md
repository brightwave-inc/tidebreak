# 2. Pre-v1 Schema and Persisted-Format Mutability

- Status: Superseded by [decision 61](0061-schema-changes-are-migrations.md)
- Date: 2026-08-07
- Owners: core
- Related: [`crates/tidebreak-core/src/db/migration.rs`](../../crates/tidebreak-core/src/db/migration.rs)
  (the single-baseline migrator),
  [`crates/tidebreak-server/src/desktop_schema.rs`](../../crates/tidebreak-server/src/desktop_schema.rs)
  (`DESKTOP_SCHEMA_EPOCH` and the reset it drives),
  [`crates/tidebreak-core/src/event.rs`](../../crates/tidebreak-core/src/event.rs)
  (the journal payload and its shape fixture),
  [`crates/tidebreak-core/fixtures/schema-baseline.sql`](../../crates/tidebreak-core/fixtures/schema-baseline.sql)
  and [`schema-baseline.postgres.sql`](../../crates/tidebreak-core/fixtures/schema-baseline.postgres.sql)
  (the rendered baseline and its drift test)

## Context

OpenWave stores local state in SQLite. Before `1.0.0` there are no deployed
databases anyone has agreed to carry forward, and the product major is pinned to
`0` — `prepare_for_product_major` refuses to run the pre-v1 lifecycle for any
other major, so this is enforced rather than assumed.

**What is true today.** The schema is one `Baseline` migration, not a chain.
`DESKTOP_SCHEMA_EPOCH` is an integer in `openwave-server`; a database written
under a lower epoch is deleted and recreated on boot, not migrated. So a schema
change is: edit the baseline in place, bump the integer. There is no
data-preserving migration to write, and no way to write one — the reset happens
before migrations run.

**Two formats, one reset.** The epoch covers the whole database file, which
includes `event.payload` — the journal rows that are the chat-history source.
The `AgentEvent` enum serialized into that column is therefore a *storage*
format, not only a wire format, and it is discarded by the same reset that
discards the tables.

**The obligation and the tripwire are separable, and were conflated.**
`event.rs` carried a pinned fixture plus tests asserting that rows written by an
older binary still parse, with a failure message directing the reader to add
`#[serde(alias)]` or write a migration. Those rows cannot exist: any change to
the payload ships with an epoch bump, which deletes them. But the fixture also
does a second job nothing else does — it makes a payload change *visible in
review*, which is what prompts the epoch bump in the first place. Nothing else
couples the two, and the failure is not graceful: `list_events` collects into
`Result`, so a single unreadable row fails an entire chat's history rather than
one message.

**Why this matters now rather than later.** Side tables have accreted that
duplicate their parent's columns and defend the duplication with composite
foreign keys and runtime drift checks (#1462). Consolidating them is a rewrite of
the baseline, which is free today and impossible after `1.0.0`.

## Decision

**Pre-v1, a schema or persisted-format change is a baseline edit plus an epoch
bump.** Both halves, in the same change. Not one bump per batch of merged work:
a contributor running `main` between two merges would otherwise hold a database
whose tables no longer match the baseline, with no reset triggered and no error
until something reads the missing column.

**Compatibility obligations for pre-v1 persisted data are suspended.** No
`#[serde(alias)]`, no backfill, and no migration is written to preserve data
that the epoch bump deletes. Tests whose only claim is "bytes an older binary
wrote still parse" are removed rather than maintained, because they assert a
property about rows that cannot reach the code.

**Change-detection tripwires are retargeted, not deleted.** The journal shape
fixture stays; its failure message directs the reader to bump the epoch instead
of adding an alias. The distinction is the whole point of this record: what is
suspended is the *obligation to stay compatible*, not the *ability to notice a
format change*. The second one is what makes the first one safe, and it is the
part that has to survive to v1.

**At `1.0.0` this inverts, in one identified place.** The fixture's failure
message flips back from "bump the epoch" to "add an alias or write a
migration", the baseline becomes the first entry in a real migration chain, and
`reset_pre_v1_state` stops being reachable. The reminder lives on the
[1.0.0 checklist](../releases.md#preparing-and-shipping-100).

Deliberately excluded: this record says nothing about *wire* compatibility
between the desktop client and server, which is a separate contract with its own
tests and is not covered by the epoch.

## Alternatives Considered

**Keep the compatibility obligations as they are.** Cheap to continue, and it
would mean no discussion. But it maintains tests that cannot fail for the reason
they claim, and it invites the wrong repair: someone adds a `serde` alias to
satisfy a red test when the correct fix is a one-integer bump. Maintaining a
guard that misdirects is worse than not having one.

**Delete the shape fixture along with the obligations.** The tidiest-looking
option, and the one "backwards compatibility does not matter pre-v1" argues for
if taken literally. Rejected because it removes the only signal that a payload
change happened at all, in a codebase where the consequence of missing it — a
whole chat's history failing to load — is silent until a user opens an old chat.
The fixture is cheap; the thing it catches is not.

**Make journal reads lossy — skip unreadable rows instead of failing the
chat — and then delete the fixture.** Genuinely defensible, and it would make
the fixture redundant rather than merely cheap. Rejected here because it is a
runtime behavior change to history rendering, decided on its own merits rather
than as a side effect of a test cleanup. If it is ever adopted, this record
should be revisited: it removes the argument for keeping the tripwire.

**Bump the epoch once per wave of schema work rather than per change.** Fewer
integer edits and a tidier history. Rejected: it is only safe if nobody runs
`main` mid-wave, which is not a property this repository has.

## Consequences

Pre-v1 schema work is cheap and destructive, and both are load-bearing. Every
epoch bump deletes local chats — for contributors, that is the cost of the
window being open, and it is why the window closes at v1.

The suspension has a hard boundary and one failure mode: a persisted-format
change that lands *without* an epoch bump. Fixtures now catch it on both sides.
`journal-events.json` and `code-journal-events.json` pin the payloads;
`schema-baseline.sql` and `schema-baseline.postgres.sql` pin the rendered DDL
for every baseline table, index, and seed row. Each one fails with a message
naming the epoch.

The table fixtures close the gap this record originally accepted, and retire
the review instruction that stood in for it. Both backends are rendered: the
epoch repairs the SQLite profile, but the durable self-host store is
PostgreSQL, and SQLite's type affinity collapses distinctions that are real
there — `uuid`, `jsonb`, and `timestamp with time zone` all render as text
variants under SQLite. A change visible only in those types would otherwise
ship unguarded, which matters more as the chain in
[`docs/releases.md`](../releases.md#preparing-and-shipping-100) replaces the
epoch.

Anything relying on local data surviving across builds cannot be built pre-v1.

Revisit this when `1.0.0` approaches — the inversion above is the work — or
earlier if journal reads become lossy.

## Validation

`the_journal_event_shape_is_pinned` fails on any change to the serialized
`AgentEvent` shape, and its message names the epoch bump as the fix.

The case a plausible wrong implementation still passes: deleting the fixture
along with the compatibility tests leaves the whole suite green, because nothing
else asserts on the payload's shape. A green suite is therefore not evidence
this record is being followed — the presence of `fixtures/journal-events.json`
and a failure message naming the epoch is.

`the_schema_baseline_is_pinned` fails on any change to the rendered baseline —
a column, an index, a check, a seed row — and its message names the epoch bump
as the fix. A baseline edit that ships without one is now a red test rather
than something a reviewer has to remember to look for.

The case a plausible wrong implementation still passes here too: regenerating
the fixture with `UPDATE_SCHEMA_FIXTURE=1` and leaving the epoch alone is
green. The fixture makes the change visible; it cannot make the decision.
