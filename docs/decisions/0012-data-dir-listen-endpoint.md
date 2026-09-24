# 12. Data-dir listen endpoint for CLI attach to a running server

- Status: Accepted
- Date: 2026-08-12
- Owners: cli, server, desktop
- Related: [`0007-cli-headless-feature-parity.md`](0007-cli-headless-feature-parity.md)
  (attach-or-embed; CLI must reach a running desktop),
  [`docs-site/content/docs/headless.mdx`](../../docs-site/content/docs/headless.mdx)
- Supersedes: none

## Context

A data directory belongs to exactly one server process. The desktop app and
`openwave serve` both bind `openwave-server` over that directory and mint a
per-launch bearer token. Decision 7 already says a second process must
**attach** as an HTTP+WS client rather than embed again.

Today attach works for `openwave serve` because it prints the URL and token on
stdout. The desktop publishes the same pair only to the webview via the Tauri
`server_info` command — nothing on disk, nothing on stdout — so the docs
honestly say attaching from the desktop is impossible. Agents and scripts that
want to drive the same profile the window owns are stuck grepping nothing, or
copying a token by hand (and the token must never ride argv).

The forces: the HTTP+WS API stays the only product surface; the bearer is full
`LocalOwner` authority and must not land in argv or shell history; the native
executor credential must stay native-only; discovery has to work for both
`serve` and the desktop without a second transport.

## Decision

**On every successful bind, the server writes a restricted listen endpoint file
into its data directory; the CLI may attach by reading that file.**

1. **File.** `{data_dir}/listen.json`, JSON object with exactly:
   - `base_url` — `http://127.0.0.1:<port>` (or whatever the process bound)
   - `token` — the per-launch bearer

   Never the client-executor token. Unix mode `0o600`, written atomically
   (temp file + rename), same pattern as `openwave-schema.json`.

2. **Lifecycle.** Overwrite on each bind. Remove on clean `Server` drop when
   practical. A stale file after a crash is allowed: the advisory lock still
   names the live owner, and a stale token simply fails auth.

3. **Writers.** Both `openwave serve` and the desktop (every `bind_*` path)
   write the file. Log-grepping `serve` stdout remains valid; the file is the
   attach path that works when nothing is printed.

4. **CLI.** `--attach` resolves the profile data directory
   (`OPENWAVE_DATA_DIR`, with the same defaults as embed), reads `listen.json`,
   and becomes `Server::Attach`. It conflicts with `--server` /
   `OPENWAVE_SERVER_URL`. The token never appears on argv. Explicit
   `--server` + `OPENWAVE_SERVER_TOKEN` remain for remote or scripted cases
   that already have the pair.

5. **Desktop attach UX.** Point `OPENWAVE_DATA_DIR` at the desktop profile
   (debug: `…/io.brightwave.openwave.dev`; release: `…/io.brightwave.openwave`)
   and run with `--attach`. No new transport, no UI “copy token” requirement.

## Alternatives Considered

- **Do nothing / document “copy from DevTools”.** Leaves decision 7's desktop
  attach gap open; unusable for agents.
- **`--server-token` on argv.** Already forbidden: process list and shell
  history.
- **Put URL/token in `openwave.lock`.** The lock file is lock-only by design;
  its contents are irrelevant.
- **Include the executor token.** Breaks the native-only boundary attach
  deliberately keeps.
- **Keychain for the per-launch token.** Heavier than a `0o600` file next to
  the lock the process already owns; the token dies with the process.
- **Auto-attach whenever the lock is held (no flag).** Surprising when the
  user meant a fresh embed under another data dir; prefer explicit `--attach`
  and a lock-failure hint that names it.
- **UDS or a second RPC.** Decision 7 keeps HTTP+WS as the product surface.

## Consequences

- Data directories gain a short-lived secret file; installers and backups should
  treat `listen.json` like a session credential (mode already restricts it).
- Attach to desktop becomes a supported headless path; headless docs must stop
  saying it is impossible.
- Stale `listen.json` after a crash can confuse a naive reader — auth failure
  plus the lock message are the recovery, not trusting the file blindly.

Revisit if a multi-user shared data directory appears (self-host already uses a
different auth model), or if OS policy forbids even `0o600` secrets beside the
DB.

## Validation

- After `bind_*`, `listen.json` exists at `0o600` with matching `base_url` /
  bearer; after `Server` drop, it is gone (or auth fails if left stale).
- `openwave --attach provider list` against a directory a `serve` owns returns
  the same catalog as `--server` + env token, with no token on argv.
- `--attach` and `--server` together are refused.
- A second embed on a locked directory still fails, and the error mentions
  `--attach` (or the listen file) rather than only stdout capture.
- The file never contains the client-executor token.

## Amended 2026-09-23

This record rejected attaching without a flag whenever a data directory's lock
is held, because a person who named another directory meant a fresh embed
there. The CLI now attaches without a flag in one case only: `TIDEBREAK_DATA_DIR`
is unset, and the desktop app's `listen.json` can be believed. A named
directory never attaches on its own, so the rejected case cannot arise. With
the variable unset, `--attach` reads the app's `listen.json`, the same default
an embed uses. See the amendment to
[decision 7](0007-cli-headless-feature-parity.md).

This record also said a stale file should not be trusted blindly, because the
lock names the live owner. The file does outlive its server: a crash, a kill,
or a signal nothing handled leaves it naming a port anything can take. So
every read of a `listen.json`, by `--attach`, by the default connection, and by
a reconnect, now requires three things before it sends anything:

- A live process holds the directory's instance lock. The reader takes a
  shared lock for as long as it takes to let go of it, and a server starting
  at that instant retries its own lock briefly rather than refusing.
- The file names a server on this computer.
- For the app's own file, the server answers `GET /version` as Tidebreak. That
  request carries no bearer, and a `404` or another program's answer stops
  the command.

`tidebreak serve` now shuts down on SIGTERM and Ctrl-C, and the desktop app
removes its file at exit. Each removes the file only while it still carries
its own bearer.

Validation: `a_client_command_connects_to_the_running_app`,
`a_listener_on_a_port_the_app_left_behind_gets_nothing`,
`a_listener_that_does_not_answer_as_tidebreak_gets_no_credential`,
`a_listen_file_naming_another_computer_is_not_followed`, and
`serve_removes_its_listen_file_when_stopped` in
`crates/tidebreak-cli/tests/serve.rs` run the binary against fake listeners
and a real `serve`. `only_a_held_lock_means_a_live_owner` in
`crates/tidebreak-server/src/listen_endpoint.rs` pins the lock check.
