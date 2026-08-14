# 16. Scoped Local Import Capability for Attached CLI Publication

- Status: Proposed
- Date: 2026-08-14
- Owners: cli, server, desktop
- Related: [`0005-cli-headless-feature-parity.md`](0005-cli-headless-feature-parity.md),
  [`0009-data-dir-listen-endpoint.md`](0009-data-dir-listen-endpoint.md)
- Supersedes: none

## Context

An attached CLI reads `listen.json` and receives the desktop server's primary
bearer. It deliberately does not receive the client-executor credential: that
second credential authorizes native host actions such as computer control and
connected-folder operations, and a process that merely knows the HTTP bearer
must not acquire those powers.

The CLI also exposes `tidebreak attach <chat> <file>`. That command reads the
file in the CLI process and sends its bytes to the chat document or image
publication route. On a desktop-owned profile those byte-ingest routes are
currently grouped with native client-executor routes, so `--attach attach`
always receives HTTP 401. The primary bearer is valid; the missing credential
is a capability the command can never obtain. Directly publishing caller-held
bytes is not a native host action, but admitting it with the full
client-executor token would collapse the boundary decision 9 protects.

## Decision

**A bound local server mints a separate, per-launch local-import capability and
publishes it only through the restricted data-directory listen endpoint.**

1. `listen.json` gains `local_import_token` alongside `base_url` and the
   primary bearer. It remains mode `0o600`, atomic, per-launch, and absent from
   argv and logs. It never contains the client-executor credential.
2. The capability authorizes only caller-supplied byte publication to an
   existing chat: raw chat-document ingest and chat image publication. It does
   not authorize client-execution claim/resolve routes, folder access, computer
   use, project/global document mutation, settings, or any other native action.
3. The scoped routes require both an authenticated principal and either the
   local-import capability or the native client-executor credential. The
   desktop's existing native publication remains valid; a bearer-only direct
   request remains HTTP 401.
4. `tidebreak --attach` reads the scoped token and sends it only on publication
   requests. Ordinary API calls continue to carry only the primary bearer.
   Explicit `--server` attachment receives no implicit local-import power.
5. The CLI continues reading the file bytes itself. The server and desktop do
   not accept a host path from the attached process and do not read arbitrary
   files on its behalf.

## Alternatives Considered

- **Give `--attach` the client-executor token.** Rejected because publication
  would accidentally grant computer, folder, and other native execution
  capabilities.
- **Let the primary bearer publish bytes directly.** Rejected because a leaked
  renderer bearer would then cross the desktop's byte-ingest boundary, and
  self-host/explicit remote clients would inherit a local-only capability.
- **Send an absolute path to the desktop for native import.** Rejected because a
  bearer client could nominate arbitrary host paths and because the CLI already
  possesses the selected bytes.
- **Add a desktop picker/approval for every CLI attachment.** Secure but turns a
  headless command into an interactive workflow and duplicates consent already
  expressed by invoking the CLI with a concrete path.
- **Do nothing.** Leaves an advertised attached-CLI command permanently
  unusable and reports the wrong diagnosis (`401` bearer failure).

## Consequences

The listen file carries one more short-lived capability and must continue to be
treated as a session-secret bundle. Route assembly becomes more explicit:
caller-byte ingest is neither general member API nor general client execution.
Clients using `--server` cannot publish local files unless a future explicit,
scoped credential handoff is designed for them. A process able to read the
profile data directory can already act as the local owner; this decision adds
only the ability to store bytes it already holds, not to make the desktop read
new host state.

Revisit if the server moves away from a single-user local data directory, if
attached clients need remote publication, or if publication gains side effects
beyond retaining caller-supplied bytes in one chat.

## Validation

- `listen.json` round-trips the primary bearer and local-import token at mode
  `0o600`, contains no client-executor token, and is removed on clean shutdown.
- An attached CLI can publish both a text document and an image into a running
  desktop chat and the next turn can reference them.
- Primary bearer alone receives 401 on both scoped publication routes.
- Primary bearer plus a wrong local-import token receives 401.
- Primary bearer plus the scoped token succeeds, while the same credentials
  still cannot claim or resolve a client-execution call.
- Primary bearer plus the native client-executor token continues to publish for
  existing desktop paths.
- Explicit `--server` behavior does not acquire the token from another source,
  and no credential appears in process arguments, logs, or error text.
