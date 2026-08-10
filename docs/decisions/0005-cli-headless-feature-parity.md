# 5. CLI/Headless Feature Parity: the Server API Is the Product Surface

- Status: Accepted
- Date: 2026-08-10
- Owners: cli, server, desktop
- Related: [`docs-site/content/docs/headless.mdx`](../../docs-site/content/docs/headless.mdx)
  (current headless documentation),
  [`docs/crates.md`](../crates.md) (crate boundaries; flags `openwave-cli` as
  a partial baseline), record 2 (pre-v1 mutability of persisted and wire
  formats)
- Supersedes: none

## Context

We want coding agents — and scripts, and CI — to drive OpenWave end to end
without the desktop app: create a chat, configure a provider, run turns that
use tools, answer approvals and plans, and read the results back. Today an
agent can get most of the way there and then hits a wall in exactly the places
an unattended run needs to keep going.

**What is true today.** The desktop app is a thin Tauri shell around an
embedded `openwave-server`; the React UI talks to it over the same HTTP+WS API
(~90 routes in [`crates/openwave-server/src/lib.rs`](../../crates/openwave-server/src/lib.rs))
that `openwave serve` exposes. Chats, turns, event streaming, models,
providers, credentials, approvals, plans, questions, permission modes, MCP
servers, plugins, and settings are all plain HTTP — none of them go through
Tauri IPC. `openwave-cli` already ships a TUI and a non-interactive print mode
(`openwave -p`) built on that API. So "parity" is not a rewrite; the engine is
already headless.

**What is not reachable headlessly.** The gaps are concentrated:

- **Unattended turns die on interaction points.** Print mode auto-rejects
  every approval ([`crates/openwave-cli/src/print.rs`](../../crates/openwave-cli/src/print.rs))
  and *cancels the turn* when the agent proposes a plan or asks a question —
  precisely the events a driving agent could answer if the CLI surfaced them.
- **Setup requires raw HTTP.** There are no CLI subcommands for provider,
  credential, model, or settings management; the routes exist but only curl
  reaches them.
- **Host folder access has no headless consent path.** Connected folders are
  brokered by `openwave-host-broker` behind a native picker/dialog. The broker
  already models an `OperatorConfig` consent method designed for headless
  provisioning ([`crates/openwave-host-broker/src/capability.rs`](../../crates/openwave-host-broker/src/capability.rs)),
  but nothing wires it up.
- **Attachments and output export are Tauri commands.** Attaching local files
  and exporting deliverables/output revisions live in
  `crates/openwave-desktop/src/{attachments,deliverables}.rs` as native
  handlers, though the underlying storage is core/server-side.
- **Local exec is macOS Seatbelt only.** A headless Linux host has no local
  sandbox; exec works only through a configured container or remote backend.

**The forces.** Every feature we route through the server API is headless for
free, forever; every feature we build as a Tauri command opens a parity gap
that must be noticed and closed by hand. The CLI must stay a client — a second
implementation of any behavior will drift. And unattended operation must not
become a permission bypass: a flag that silently auto-approves everything is a
footgun, but a headless mode that cannot approve anything is useless for the
stated goal.

## Decision

**1. The server HTTP/WS API is the canonical product surface; parity is
defined against it.** A feature has CLI/headless parity when it is reachable
through `openwave-server` routes and exercised by `openwave-cli`. New features
land as server routes first; a Tauri command is reserved for things that
genuinely require the native shell. The allowed native-only set is closed and
explicit: OS file/folder pickers, keychain and code-signing plumbing, the
auto-updater, native window/attention control, office-converter and Node
runtime installers, and local voice capture. Anything else appearing as a
Tauri command is a parity bug.

**2. Print mode becomes a real unattended protocol, not a demo.** `openwave
-p` gains:

- `--permission-mode ask|auto|allow|plan` per invocation, mapping onto the
  existing per-chat permission modes — no new permission vocabulary.
- Structured NDJSON I/O (`--output-format json` today, plus stdin): approval
  requests, plan proposals, and user questions are emitted as events, and the
  driving process answers them by writing decision lines back. A turn only
  blocks on interaction when the caller opted into interactive mode;
  otherwise the standing policy answers.
- Non-interactive defaults that are safe and stated: in `ask` mode with no
  driver attached, approvals are rejected (today's behavior) and plans/
  questions terminate the turn with a distinct exit code and a machine-readable
  reason — never a silent cancel.

**3. Setup is scriptable through CLI subcommands that wrap existing routes.**
`openwave provider|model|settings|mcp` subcommand families cover credential
set/remove, provider status, model listing/selection, exec-backend and
web-search configuration. These are thin clients of the HTTP API; they contain
no logic of their own. Credentials keep flowing through the OS keychain
exactly as they do today — headless changes the entry path, not the storage.

**4. Folder access gets the operator consent path the broker already
anticipates.** `openwave folder connect <path>` (and `list`/`disconnect`)
records grants via `ConsentMethod::OperatorConfig`. This is deliberate,
explicit provisioning by whoever controls the machine — the CLI never
auto-grants a folder because an agent asked for it mid-turn. Mid-turn folder
requests in headless mode fail with the same typed refusal an undecided
desktop prompt produces.

**5. Attachments and output export move to the server surface.** File
attachment takes a path argument and feeds the same ingest routes the desktop
uses; deliverable/output listing, reading, revision history, and export-to-path
become server routes plus CLI subcommands, replacing the desktop's
`Store`-direct Tauri handlers with calls to the same new routes so both shells
share one implementation.

**6. Attach or embed, explicitly.** The CLI keeps its current embed-a-server
default (own data dir, isolated instance — the right shape for agents trying
things out), and additionally accepts `--server <url>` / `OPENWAVE_SERVER_URL`
plus a token to act as a pure client of an already-running `openwave serve` or
desktop instance. Two processes embedding servers over one data dir remains
unsupported; the CLI refuses when it can detect it rather than corrupting
quietly.

**Deliberately excluded:** a headless local sandbox for Linux/Windows (exec on
headless hosts requires a container or remote backend, and the CLI must say so
clearly when unconfigured — this is [`docs/deferred.md`](../deferred.md)
territory, not a parity gap to paper over); any GUI-preview equivalents
(rendered document/chart viewers — headless consumers read files); the
auto-updater; local voice; and a second wire protocol (no gRPC, no bespoke
daemon socket — HTTP+WS is the only transport).

## Alternatives Considered

**Do nothing — document the HTTP API and let agents curl it.** The engine is
already reachable; a determined script works today. Rejected because the
blocking gaps are not ergonomic but functional: plans and questions *cancel*
turns in print mode, folder consent has no headless path at all, and every
consumer would reimplement event-stream handling and approval plumbing. The
API-only story also leaves no pressure against new features landing as Tauri
commands.

**Drive OpenWave through its MCP face instead of a CLI.** `openwave mcp`
exists and agents speak MCP natively. Rejected as the parity vehicle: the MCP
server exposes two read-only tools scoped to one workspace, and modeling
chat/turn/approval lifecycles as MCP tools would be a second, weaker API over
the first. The MCP face stays what it is — a way for *other* agents to use
OpenWave's tools — and may grow later, but parity is defined against HTTP.

**A fatter CLI that links the engine crates directly** (bypass the server,
call `openwave-core` in-process). Rejected: it forks behavior the moment
server routes gain logic (auth, workers, connectors already live in
`openwave-server`), and it makes the CLI a second implementation to keep
honest. The CLI stays a client even when it embeds the server in-process.

**Auto-approve-everything flag for unattended runs** (`--yolo`). Simpler than
NDJSON driving, and other tools ship it. Rejected as the *only* mechanism: it
collapses the permission model instead of exercising it, and the stated goal
is agents that answer approvals, not agents that never see them. `allow` mode
per invocation covers the legitimate cases with vocabulary we already have.

**Headless folder grants on demand** (agent requests a folder, CLI grants it
because a flag said yes). Rejected: folder access is host-machine consent, and
the broker's own design separates operator provisioning from in-turn requests.
Collapsing them would make every headless run a standing grant to anywhere the
agent wanders.

## Consequences

- The HTTP/WS API graduates from internal seam to supported contract. Under
  record 2's pre-v1 rules it may still change freely, but changes now break
  scripts and agents, not just our own two clients — route changes deserve the
  same care as persisted formats, and an OpenAPI description (today the route
  table is the spec) becomes worth generating.
- Desktop work gets a standing constraint: features land as server routes
  first. The closed native-only list must be enforced in review; the parity
  check below makes drift visible.
- Moving deliverable/attachment handling from Tauri handlers to server routes
  is real migration work in the desktop, not just CLI addition.
- The NDJSON driving protocol becomes a wire contract of its own — versioned
  with the same discipline as the event journal shapes it re-exposes.
- Operator folder grants create a new provenance of standing grant that
  security review must treat as first-class (audit surface, revocation,
  display in the desktop's connected-folders UI).
- Exec on headless Linux remains backend-dependent; CI and agent harnesses
  must provision a container backend, and that setup cost is now on the
  critical path of "agents try stuff out."
- Revisit if: a non-HTTP consumer appears with needs WS cannot meet (would
  reopen the single-transport rule); the MCP face grows a real lifecycle
  surface (would reopen the parity-vehicle choice); or a supported local
  sandbox lands for Linux (would shrink the excluded set).

## Validation

- End-to-end headless turn: `openwave -p` in `allow` mode runs a real
  tool-using turn on a fresh data dir and the journal records the completed
  turn — the highest-value single test, per the testing bar.
- Interaction protocol: a driven `-p` session receives a plan-proposal event
  and answers it over stdin; the turn *continues* rather than cancelling. A
  wrong implementation that merely auto-accepts plans would pass a
  "turn completed" assertion — the test must assert the decision came from the
  driver's stdin line, not from policy.
- Undriven `ask`-mode run that hits a question exits with the documented code
  and machine-readable reason; asserting on exit status alone would let a
  silent-cancel regression pass, so the reason payload is part of the
  assertion.
- Parity gate: a check that the set of `#[tauri::command]` handlers is a
  subset of the explicit native-only allowlist, so a new desktop-only feature
  fails CI until the list is consciously amended or the feature gets routes.
- Folder consent: an `OperatorConfig` grant made by the CLI is visible and
  revocable from the desktop's connected-folders UI against the same data dir,
  and a mid-turn folder request in headless mode produces the typed refusal,
  never a grant.
- Setup subcommands are validated by their routes' existing coverage — they
  are thin clients, and tests that re-walk each route through the CLI would be
  duplicate coverage under the testing policy; one smoke test that a
  credential set via CLI is immediately usable by a turn suffices.
