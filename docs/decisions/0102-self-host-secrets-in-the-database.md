# 102. Self-host secrets in the database

- Status: Accepted
- Date: 2026-09-24
- Owners: server, security
- Related: [0006](0006-self-host-deployment-plane-authorization.md) (who may
  write the deployment's secrets, unchanged here),
  [0061](0061-schema-changes-are-migrations.md) (migration
  `m20260924_000005_deployment_secrets`),
  [`crates/tidebreak-server/src/database_secrets.rs`](../../crates/tidebreak-server/src/database_secrets.rs),
  [`docs/self-hosting.md`](../self-hosting.md#secrets-in-the-database)
- Supersedes: none

## Context

A self-host deployment stores provider, web-search, code-execution, and
connected-app credentials through its admin-only deployment plane (decision
6). Until now the only place it could keep them was HashiCorp Vault KV v2.
Without Vault the server reads provider environment variables and refuses
every credential write, so a team that does not run Vault cannot save a key
through the app. The deployment already has a PostgreSQL database that its
operator runs and backs up.

## Decision

When `TIDEBREAK_SECRET_KEY_FILE` is set, the self-host profile keeps its
stored secrets as encrypted rows in its own database.

- The file holds a 32-byte key as one line of standard base64; trailing
  whitespace is ignored. The server reads it once at boot, before it opens the
  database, and refuses to start when the file is missing, unreadable, or does
  not decode to exactly 32 bytes.
- The `deployment_secrets` table holds one row per secret: `name` (primary
  key), `key_id`, `nonce`, `ciphertext`, and `updated_at`. The profile stores
  its credentials as one bundle, so in practice the table holds one row.
- Each value is encrypted with AES-256-GCM through `ring::aead`, under a fresh
  random 96-bit nonce from `ring::rand::SystemRandom` for every write. The
  associated data is `tidebreak-secret-v1:` followed by the row's name, so a
  ciphertext copied to another name, or altered in place, fails to decrypt.
- `key_id` is the first 8 bytes of the key's SHA-256, in hex. At boot the
  server refuses to start when any row carries another `key_id`, and says to
  restore the original key file. No write replaces or deletes a row written
  under another key.
- A row that fails to decrypt is an error that names the secret, never an
  unset secret. Errors and logs name secrets, never values or the key.
- Setting both `TIDEBREAK_SECRET_KEY_FILE` and the `TIDEBREAK_VAULT_*`
  variables is a configuration error. With neither, nothing changes:
  environment variables still work as fallbacks, and writes fail with setup
  guidance. The desktop profile rejects the variable and keeps the OS
  keychain.

This changes where secrets are stored, not who may write them. The deployment
plane of decision 6 remains the only writer, and the rows sit outside the
`Store` trait that request handlers use.

Excluded: key rotation, more than one active key, a key held in an external
key management service, and moving secrets between Vault and the database.

## Alternatives Considered

**Vault as the only store.** Rejected: it makes running a second service a
prerequisite for saving any credential.

**Plaintext rows.** Rejected: every dump, replica, and backup of the database
would carry every provider key.

**A passphrase in an environment variable.** Rejected: environment variables
leak through process listings, crash reports, and container inspection more
readily than a mounted file, and deriving a key adds a key-derivation function
and its parameters for nothing over 32 random bytes.

**Disk or database encryption alone.** Complementary, not a substitute: it
protects a stolen disk, not a `pg_dump` taken by anyone who can reach the
database.

## Consequences

A database dump or backup alone reveals no secret. Whoever holds both the key
file and the database reads every secret, and that includes anyone who can
read the running server's memory or mounted files. Losing the key loses the
secrets; each must be entered again. Operators therefore back the key up apart
from the database, so one stolen backup never holds both.

Vault stays an option because it offers what a key file cannot: access
policies, an audit log, short-lived tokens instead of a long-lived key on the
server, and rotation of its own. A deployment picks one of the two.

Key rotation is future work. It needs the server to accept a second key while
it re-encrypts each row under the new one. Until then, replacing the key means
deleting the rows and entering the secrets again. Random 96-bit nonces stay
safe until about 2^32 writes under one key, far beyond what a deployment makes.

Revisit when a deployment needs rotation or a key held in a key management
service.

## Validation

Unit tests on SQLite cover a round trip, an overwrite, a delete, a ciphertext
moved to another name, a tampered ciphertext, tag, and nonce, the wrong key
refusing to open while changing nothing, rows under another key surviving
writes and deletes, both custody options set, the desktop profile rejecting
the variable, and each key file case: missing, unreadable, empty, not base64,
16 and 33 bytes, oversized, and trailing whitespace accepted. The PostgreSQL
lane runs the same flow against a fresh database and boots the server through
`tidebreak_server::bind`, including the refusal under the wrong key file. An
implementation without associated data fails the moved-ciphertext test. One
that checks the key only on read boots under the wrong key and fails the boot
test.
