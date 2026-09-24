# 102. Self-host secrets in the database

- Status: Accepted
- Date: 2026-09-24
- Owners: server, security
- Related: [0006](0006-self-host-deployment-plane-authorization.md) (who may
  write the deployment's secrets, unchanged here),
  [0061](0061-schema-changes-are-migrations.md) (migration
  `m20260924_000005_deployment_secrets`),
  [`crates/tidebreak-server/src/database_secrets.rs`](../../crates/tidebreak-server/src/database_secrets.rs),
  [`docs/self-hosting.md`](../self-hosting.md#secrets-in-the-database),
  #3590 (keeping members' code sessions away from the deployment's secrets)
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
  not decode to exactly 32 bytes. It also refuses a file that its group or
  other accounts can write, because on an empty table another local account
  could plant a key it knows. It warns, but starts, when they can only read
  it. The check applies to the file a symlink names, since Kubernetes mounts
  secrets through symlinks.
- The `deployment_secrets` table holds one row per secret: `name` (primary
  key), `key_id`, `nonce`, `ciphertext`, and `updated_at`. The profile stores
  its credentials as one bundle, so in practice the table holds one row.
- Each value is encrypted with AES-256-GCM through `ring::aead`, under a fresh
  random 96-bit nonce from `ring::rand::SystemRandom` for every write. The
  associated data is `tidebreak-secret-v1:` followed by the row's name, so a
  ciphertext copied to another name, or altered in place, fails to decrypt.
- `key_id` is the first 8 bytes of the key's SHA-256, in hex. At boot the
  server refuses to start when any row carries another `key_id`. The refusal
  says to restore the original key file, and, if that key is lost, to delete
  the rows written under it and enter the credentials again. No write replaces
  or deletes a row written under another key.
- At boot the server also decrypts every row once, and refuses to start when
  one no longer decrypts, as after a damaged restore. Otherwise a caller that
  asks whether a credential exists reads the failure as "none", and the
  deployment looks unconfigured. The refusal names the secret and says to
  restore the database or delete that row.
- After boot, a row that fails to decrypt is an error that names the secret,
  never an unset secret. Errors and logs name secrets, never values or the
  key.
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

The design protects dumps and backups, not a database someone can write to.
The associated data binds a row to its name, not to a version, so a writer can
put back an older row under the same name and key, and it still decrypts:
credentials that were replaced or removed come back. Such a writer can already
run code on the server through stored MCP server commands, so the store does
not try to defend against one.

This change does not widen an exposure that exists today. A self-host member
who can use Code mode can read the key file and the database URL: workspace
terminals and coding engines run as the server's uid (10001) and inherit the
server's environment, and the terminal routes are on the member plane. The
Vault token file and provider environment variables are exposed the same way.
Until members' code sessions are kept away from the deployment's secrets
(#3590), give self-host accounts only to people you would trust with those
secrets.

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
refusing to open while changing nothing, a row under the right key that no
longer decrypts refusing to open, rows under another key surviving writes and
deletes, both custody options set, the desktop profile rejecting the variable,
and each key file case: missing, unreadable, empty, not base64, 16 and 33
bytes, oversized, trailing whitespace accepted, writable by group or others
refused, readable by them loaded with a warning, and a symlink judged by its
target. The PostgreSQL lane runs the same flow against a fresh database and
boots the server through `tidebreak_server::bind`, including the refusals
under the wrong key file and over a damaged row. An implementation without
associated data fails the moved-ciphertext test. One that checks the key only
on read boots under the wrong key file, and one that checks key ids alone
boots over the damaged row; each fails its boot test.
