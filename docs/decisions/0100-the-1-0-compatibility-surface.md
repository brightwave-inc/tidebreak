# 100. The 1.0 compatibility surface

- Status: Accepted
- Date: 2026-09-23
- Owners: core, desktop, CLI
- Related: [`0061-schema-changes-are-migrations.md`](0061-schema-changes-are-migrations.md)
  (the append-only chain this record keeps through 1.0),
  [`0007-cli-headless-feature-parity.md`](0007-cli-headless-feature-parity.md)
  (the CLI and `-p` surface),
  [`0073-agent-mcp-drives-chat-over-attach.md`](0073-agent-mcp-drives-chat-over-attach.md),
  [`0074-agent-mcp-drives-code-mode.md`](0074-agent-mcp-drives-code-mode.md),
  [`crates/tidebreak-server/src/desktop_schema.rs`](../../crates/tidebreak-server/src/desktop_schema.rs),
  [`crates/tidebreak-core/src/db/backup.rs`](../../crates/tidebreak-core/src/db/backup.rs),
  [1.0.0 checklist](../releases.md#preparing-and-shipping-100),
  [`SECURITY.md`](../../SECURITY.md)
- Supersedes: the 1.0 squash and the product-major guard in decision 61, and
  the supported-contract status decision 7 gave the HTTP and WebSocket API

## Context

Tidebreak is at 0.114 and plans a 1.0 release. A 1.0 is a promise about what
keeps working across 1.x releases, and until now nothing wrote that promise
down. Three things stood in the way.

**A 1.x build could not open any profile.** The desktop schema guard refused
every product major other than `0`, so a `v1.0.0` app would have failed to
start on every machine, including a fresh one.

**The plan for 1.0 deleted data.** Decision 61 and the 1.0.0 checklist planned
to squash the migration chain into one new baseline at 1.0. SeaORM records each
migration by name, and a squash renames the first one. Every 0.x database would
then record names the 1.0 build does not know, and the only way through is to
reset them. Local data has survived upgrades since v0.61.0, so that plan would
have broken the one data promise the product already keeps.

**Some paths still deleted a profile outright.** A database with no schema
marker, or one below the migration pin, was deleted with no copy. Migrations
ran with no backup, and a downgrade failed with SeaORM's "Migration file of
version … is missing", which reads like a broken install. The update dialog
still warned that any update may wipe all Tidebreak data, which has not been
true since v0.61.0.

The interfaces around the data are uneven too. The CLI, its `-p` stdin decision
protocol, and `tidebreak agent-mcp` are the surfaces scripts and agents drive.
The HTTP and WebSocket API is a lockstep contract: every vocabulary is closed,
every frame rejects unknown keys, and the renderer ships with its server
(`crates/tidebreak-server-api/src/wire.rs`). Nothing versions it, and the
mobile client carries its own copy of the generated types.

## Decision

**Local data.** Every 1.x release upgrades, in place, any profile written by
v0.61.0 or later:

- The migration chain stays append-only through 1.0 and after. It is never
  squashed, and `LAST_RESET_EPOCH` stays at 41. Builds with product major `0`
  and `1` share one lifecycle. A later major is refused until it defines its
  own upgrade path.
- Before the desktop app migrates an existing SQLite database, it copies the
  database with `VACUUM INTO` to
  `<data_dir>/backups/pre-migration-<version>-<timestamp>.db`. The two newest
  copies are kept. If the copy fails, no migration runs.
- A database that records a migration this build does not know came from a
  newer build. Opening it fails before anything changes, with: "This Tidebreak
  profile was written by a newer version. Install that version or later, or
  restore a backup from <backups path>." PostgreSQL gets the same refusal
  without the path.
- Nothing deletes a profile. A profile below the pin, a database whose
  migrations are not a prefix of this build's chain past the baseline, and
  journals left without a database are moved into
  `<data_dir>/backups/unrecognized-<timestamp>/`, with their document bytes,
  the host broker's durable files, and a copy of the schema marker. The move
  is logged at warning level with the path, and a fresh profile starts.
- A database with no marker whose recorded migrations are a prefix of the chain
  past the baseline keeps its data and gets a fresh marker.
- PostgreSQL operators own their backups. The server takes none.

**Stable through 1.x.** A 1.x release keeps these compatible. A change that
breaks one is a major release.

- CLI commands and their flags, as the CLI's usage text and the
  [headless docs](../../docs-site/content/docs/headless.mdx) list them.
- CLI exit codes, per command. Every command exits `0` on success, `1` on
  failure, and `2` on a usage error. `-p` adds `3` (an interaction nobody
  drove), `4` (a decision that could not be applied), and `130` (interrupted).
  `code` adds `3` (`run --on-approval fail` parked on an approval), `124`
  (timed out), and `130`.
- CLI JSON output (`--output-format json` and `--json`): existing fields keep
  their names, types, and meaning. New fields may appear, so a reader must
  ignore fields it does not know.
- `tidebreak agent-mcp` tool names and input schemas, and its result shape
  (`status`, `assistant_text`, `pending`, `events_cursor`). New tools and new
  optional inputs may appear.
- The `-p` stdin decision protocol: the `"tidebreak":"v1"` control events on
  stdout and the decision lines on stdin, as `crates/tidebreak-cli/src/print/protocol.rs`
  defines them.

**Internal until a versioned API exists.** The HTTP and WebSocket API,
including the `/chats/*` and `/sessions/*` event frames, stays an internal
contract between a server and the clients that ship with it. It may change in
any release. The `-p --output-format json` stream passes those event frames
through as the server sends them, so a script keys on the CLI's own
`"tidebreak":"v1"` events, not on the frames.

**Platforms.** macOS is supported now. Windows and Linux builds return before
the `1.0.0` tag, and the 1.0.0 checklist verifies them.

Deliberately excluded: a stability promise for the HTTP and WebSocket API, the
mobile client's wire copy, the sandbox-agent protocol, and anything a person
builds on files inside the data directory other than the backups this record
describes.

## Alternatives Considered

**Squash the chain at 1.0, as decision 61 planned.** A tidier first release,
and fresh databases would build from one baseline instead of every step since
August. Rejected: the squash renames the first migration, so every 0.x profile
records names 1.0 does not know. Keeping that data would need a name-mapping
table, which is the chain again under another name, and dropping it breaks the
v0.61.0 promise.

**Keep the product-major guard until more of the checklist lands.** The status
quo. Rejected: the guard existed so a stable build could never run the pre-v1
reset. That reset is gone, because nothing deletes a profile now, so the guard
would only stop a 1.x build from starting.

**Keep resetting unrecognized profiles.** One fewer code path, and a
marker-less database was rare. Rejected: rare is not never, the reset kept no
copy, and a marker deleted by hand is exactly the case a person cannot debug.

**Copy the database on every boot.** Simpler to reason about than "only before
a migration". Rejected: `VACUUM INTO` rewrites the whole file, a real cost on a
large profile at every launch, for no gain. Only a migration changes the schema.

**Back up PostgreSQL from the server.** Rejected: the server would need
storage, credentials, and a retention policy that operators already have in
their own tooling, and a dump from inside the server competes with it for the
database it serves.

**Promise the HTTP and WebSocket API at 1.0, as decision 7 anticipated.**
Rejected for now: the contract is deliberately strict and lockstep, and a
stability promise would freeze internals the desktop and mobile clients evolve
with each release. A versioned API is separate work with its own design.

## Consequences

- The chain only grows. A fresh database runs every migration, and each new
  schema change has to reach every shape a v0.61.0 or later build could have
  left, as decision 61 already requires.
- The data directory gains `backups/`. It holds up to two pre-migration copies,
  each about the size of the database, and any `unrecognized-*` folders, which
  nothing prunes. People delete those by hand.
- A downgrade is refused, never attempted. Going back to an older build means
  restoring a copy that build can read.
- The CLI, `-p`, and `agent-mcp` surfaces now carry the same obligation the
  persisted data does: a change that breaks a documented command, flag, exit
  code, JSON field, tool, or protocol line waits for a major release.
- Exit code `3` means different things to `-p` and `code run`. That stays as it
  is, because changing it now is the kind of break this record rules out.
- Revisit when a versioned HTTP API ships, when a 2.0 is planned, or if the
  chain's length becomes a measurable cost at startup or in tests.

## Validation

- `a_major_one_build_opens_a_0x_profile_and_keeps_its_data` and
  `an_unknown_future_major_is_refused_without_touching_the_profile`
  (`crates/tidebreak-server/src/desktop_schema.rs`) pin the major guard.
- `a_database_without_a_marker_keeps_its_data_when_it_recorded_the_chain`,
  `a_database_without_a_marker_that_this_build_cannot_read_is_moved_aside`,
  `journals_left_without_a_database_are_moved_aside_before_a_fresh_start`, and
  `a_profile_below_the_pin_is_moved_aside_with_its_data` pin that nothing is
  deleted. The last one opens the folder on its own and finds the chat.
- `only_a_chain_prefix_past_the_baseline_is_carried_forward` pins the
  marker-less rule, including the baseline-only database a build before the
  pin leaves behind.
- `a_database_with_pending_migrations_is_copied_before_it_migrates`,
  `only_the_newest_copy_and_the_one_before_it_are_kept`, and
  `a_database_from_a_newer_build_is_refused_and_left_alone`
  (`crates/tidebreak-core/src/db/tests/pre_migration_backup.rs`) pin the copy,
  its retention, and the downgrade refusal.
- The tool-name test in `crates/tidebreak-cli/src/agent_mcp/mod.rs` and the
  protocol tests in `crates/tidebreak-cli/src/print/protocol.rs` pin the
  `agent-mcp` and `-p` surfaces.

The case a plausible wrong implementation still passes: an appended migration
that only works on a fresh database is green in every test that starts from
one. `a_stepwise_upgrade_lands_on_the_fresh_schema` and the versioned upgrade
fixtures catch it; a migration test that starts from a fresh database does not.
