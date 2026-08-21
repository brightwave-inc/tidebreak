# 56. One credential item per profile

- Status: Accepted
- Date: 2026-08-21
- Owners: desktop, server
- Related: [`0016-desktop-staging-channel.md`](0016-desktop-staging-channel.md)
  (per-channel keychain services),
  `crates/tidebreak-core/src/secret_bundle.rs`,
  `crates/tidebreak-server/src/secret_rehome.rs`

## Context

macOS grants keychain access on the code signature of the binary that *created*
an item. An app update replaces the binary, so every credential the previous
build wrote is stranded: reads prompt, and a gateway session that fails to read
presents as "not connected".

`secret_rehome` already repairs that by rewriting each value from the new
binary. The repair works, but it is per item, and so is the prompt. A profile
holding four credentials — two provider keys, the ChatGPT sign-in, the gateway
session, which is what a real desktop profile looked like on 2026-08-21 —
costs roughly eight approvals per update: `rehome_one` reads, deletes, stores,
and reads back, and the first two of those hit an item the new binary does not
own yet. The `tidebreak.log` on that machine records the pass running twice in
two days, each time reporting four credentials.

`CachingSecretProvider` removes *repeat* reads within a process. It cannot
remove the first read of each distinct item, which is where the prompt is. The
number of prompts is the number of items, by construction.

`static_secret_keys` enumerates eighteen fixed keys before per-record MCP and
connected-app keys, so the ceiling grows with the feature set rather than
staying at four.

## Decision

Every logical key in a profile is stored inside **one** keychain item,
`tidebreak.secret_bundle_v1`, holding a JSON object of key → value.
`BundledSecretProvider` is a `SecretProvider` decorator, so nothing above it
learns where a key sits; callers keep naming
`provider.anthropic.credential`. The wrap order at boot is
`Caching(Bundled(Keychain))`.

Three rules make it safe:

- **Reads fall back to a per-key item of the same name.** Migration is a
  boot-time pass, and the shell's remote attachment reads its token before that
  pass can have run. Without the fallback, the launch after an update would read
  as "not attached". The fallback makes ordering stop mattering.
- **Writes merge against the store.** A write takes a process-wide lock,
  re-reads the item, applies its change, and confirms the read-back; a store
  that changed underneath is merged again rather than overwritten. The desktop
  builds two providers over one item — the server's and the shell's — and
  `tidebreak rehome-secrets` deliberately runs without the instance lock.
- **Migration stores before it removes.** The sweep writes a value into the
  bundle and reads it back *before* touching the old item. The
  "removed, then could not store it again" state that the per-item repair can
  reach does not exist for it.

`secret_rehome` becomes: re-home the one item, then sweep any leftover per-key
items into it. `desktop.remote-machine.token` joins the swept set; it is a real
stored credential that the enumeration never named, so it was never re-homed.

Deliberately excluded: encrypting the JSON (the item is already protected), and
deleting the item when it becomes empty (a churned item buys nothing).

## Alternatives Considered

**Do nothing.** The prompts are survivable and the repair already works. Rejected
because the count scales with the feature set, not with anything the user did,
and the most expensive prompt — the gateway session — is the one whose failure
presents as a silent sign-out.

**Keep per-key items, trim the re-home pass.** Skipping the read-back, or
detecting "already owned" before rewriting, roughly halves the prompts. Rejected:
it is the item count that sets the floor, and this leaves it untouched.

**Put the credentials in SQLite, encrypted with one keychain-held key.** One item
by another route, and a smaller item. Rejected: it moves secrets into the store
that `SecretProvider` exists to keep them out of, and a pre-v1 schema-epoch reset
deletes that database.

**A macOS-specific ACL that trusts the application.** `SecAccessCreate` with the
app in the trusted list would stop the prompts without changing the layout.
Rejected: `keyring` does not expose it, it is one platform only, and it does not
help the dev-signing case that motivated `secret_rehome` in the first place.

## Consequences

An update costs one approval rather than one per credential, and the cost stops
growing as features add keys.

What it costs:

- **A lost-update window across processes.** Merge-on-write plus
  read-back-and-retry narrows it to the write itself; it does not close it.
  Writes are rare — credential edits and sign-in — and the daemon holds the
  instance lock.
- **A one-way migration.** A build older than this one sees no per-key items and
  reads every credential as absent. Downgrading means re-entering credentials.
  The pre-v1 data warning in the updater already covers the user-facing case.
- **One item is one failure domain.** An undecodable bundle is every credential
  at once, which is why the decoder refuses rather than treating the item as an
  empty profile — reporting "no credentials" would look like a fresh install and
  then overwrite the one thing a reader could recover from.
- **Two extra cheap reads per absent key.** A miss re-reads the item and then
  asks for a per-key item. Absent items answer without prompting, and the outer
  cache absorbs the repeats.

Revisit if a credential grows large enough to strain a generic password item, if
concurrent writers stop being rare, or if a platform gains a way to grant an
application durable access to an item across signatures — which would remove the
prompt without needing the layout at all.

## Validation

`crates/tidebreak-core/src/secret_bundle.rs` asserts the property directly:
storing three credentials leaves the store holding exactly one item. Alongside
it: two providers over one item do not lose each other's keys; a key still in
its own item reads before migration runs; a delete reaches the leftover item, so
the fallback cannot resurrect it; and an undecodable item is refused rather than
overwritten.

`crates/tidebreak-server/src/secret_rehome.rs` asserts that a migrated profile
re-homes exactly one item, by recording the operations the pass performs — a
plausible wrong implementation that swept correctly but still rewrote each key
would pass a value-level assertion and fail this one. It also pins that a
per-key item which cannot be removed is reported as skipped and never as lost.

What none of that covers is the real backend: the tests use an in-memory store,
and the prompt behaviour lives in macOS. That was checked by hand against a
scratch keychain service — two per-key items in, one bundle item out, values
intact, and a second pass touching only the bundle.
