# What comes after v1

Tidebreak's first stable release is deliberately narrow: a local-first coworker
that can work over user-approved files, use configured models and bounded
tools, run code in an isolation boundary, and produce files the user explicitly
exports. The work below is valuable, but it is not silently implied by that
promise.

This is the canonical record of deliberately parked product scope. It is not a
ticket queue and it does not make dates or release promises. A concrete task
with an owner belongs in an issue; a long-horizon capability belongs here until
the conditions for building it are real.

## Completing the v1 foundation

Several pieces are ordinary implementation work, not deferred product bets.
They stay as open issues without a `deferred` label because a contributor can
pick them up now:

- Close the remaining multi-principal prompt-inbox ownership gap in the
  self-hosted profile.
- Build the egress topology that can enforce Docker policies other than the
  current fail-closed `--network none` posture.

These are important finishing work, but they do not change the product's basic
shape. They should be delivered as normal, reviewable slices rather than held
in a parking label.

## A client for self-host teammates

The self-host profile already has named users and an admin/member split on the
API. What a teammate runs today is that API and the `tidebreak` CLI with a
static bearer token from the operator file. The packaged desktop app stays a
local Desktop-profile product: it embeds its own server and does not point at
a remote deployment.

The desktop app can attach to a remote machine and reports that attachment
through `remote_machine_state`. The hosted web UI is unparked:
[decision 82](decisions/0082-the-hosted-machine-serves-the-renderer.md) has
the machine serve the desktop renderer to browsers, with a session that lasts
its bearer. Refreshing a browser session without re-entering through the
gateway console stays parked until someone keeps a tab open past an hour and
minds. The supervision-first mobile client is unparked as #2644.
Auth beyond the static token file is sequenced there too:
roster-provisioned tokens first, a gateway authenticator behind decision 6's
credential-to-principal seam as the end state.

## A coworker that can work later

Background agent runs make work durable, but Tidebreak does not yet schedule
work for a future time or recurring cadence. A future local-first scheduler
should reuse the run journal and admission path rather than create a second
automation engine. Its first useful shape is deliberately modest: one-time,
daily, and weekly tasks; explicit enable/disable controls; visible history; and
bounded catch-up after the computer has been asleep or offline.

Scheduling does not create blanket consent. A task that reaches an external
effect still needs an applicable approval, and future exact-target recurring
grants must stay constrained to a tool's canonical destination. Arbitrary cron,
cloud execution while the user's machine is unavailable, and a separate agent
runtime are outside that first step.

## Browser work beyond the macOS release

The visible Tidebreak-managed browser is active implementation work under
#2335. Foreground chat and local code harnesses share the native control
boundary. Release acceptance remains in #2345; see
[code-mode browser integration](code-browser-integration.md) for the working
contract and validation steps.

Windows and Linux agent control, background browser agents, arbitrary signed-in
services, personal-profile access, CAPTCHA handling, and password-manager
integration remain deferred. Desktop computer use stays a separate capability
with per-app grants under
[decision record 13](decisions/0013-computer-use-screen-capture-and-app-control.md).

## More ways to organize and shape work

The basic chat remains useful on its own. Later work can make it easier to
organize and direct:

- Search inside an already approved connected folder from the composer, using
  root-relative paths and bounded discovery rather than exposing absolute host
  paths.
- Let a reusable plugin provide an output template as well as a prompt or
  skill, once there is a settled way to deliver a template into a turn.
- Install instruction-only plugins from public skill indexes, with explicit
  updates and no background auto-update. Pinned Git install is in the
  Plugins page and `tidebreak plugins install`.
- Eventually admit capability-bearing plugin components packaged as MCPB
  bundles, only with component-level consent, keychain-backed configuration,
  and enforced tool-schema validation.

The marketplace and executable-plugin work follows the simpler
instruction-only pipeline; it is not a v1 dependency.

## Connected services: local control first, managed entitlements later

Tidebreak already has local connected folders, configured MCP servers, web
search, and a governed REST executor for local apps. It does not plan to become
the publisher and refresh-token broker for a parallel catalog of Slack, Google,
Microsoft 365, Dropbox, or Box integrations.

Where a service requires a registered OAuth app, the intended managed path is a
model gateway entitlement. Tidebreak should consume those entitled apps or
virtual MCP endpoints and explain what a gateway provides, while unmanaged
users retain local escape hatches such as MCP mounting and user-provided REST
definitions and credentials. Curated desktop-owned OAuth connectors are not on
the current path. The MCP directory in Connected apps is not such a catalog: it
lists servers their vendors host, each runs its own sign-in, and Tidebreak
holds no OAuth app and runs no token service for them.

Two related capabilities wait on clear boundaries: choosing governed REST
operations to expose as foreground chat tools, and making gateway-attested MCP
execution modes visible in Settings. The former needs its own model-tool,
approval, snapshot, and audit contract; the latter waits for the gateway to
expose execution-mode metadata. Recording a gateway identity beside turns and
host-access audit events is likewise a future attribution feature, not local
access control.

## Deeper isolation and reliability

Tidebreak can run code through local and managed execution providers today. The
more ambitious sandbox-resident agent-run tier remains attached-only and
opt-in. Detached background execution needs scoped model tokens, provider
lifetime caps, image verification in the right trust root, no
host-authority-reachable tools, and enforceable credential egress rules. Until
those properties hold together, the in-process durable run path is the
supported one.

Because that tier is parked, the `sandbox-resident container e2e` CI lane has
been removed rather than left failing against functionality nobody is
advancing. The loopback tests still drive the same host driver against the real
sandbox agent over a socket on every run; what is no longer proven is the
Docker packaging and the container network boundary. Restoring that lane is
part of picking the tier back up, not a separate task.

## Native Vertex AI and Bedrock routes

The first-class Google Vertex AI and Amazon Bedrock Mantle providers were
removed, along with the service-account and AWS access-key credential types
that existed only to serve them. Google service accounts and AWS SigV4 are the
only credential shapes Tidebreak ever carried that are not an API key in the OS
keychain, and every model those routes served is already reachable — through a
direct Anthropic or Google API key, or through an OpenAI-compatible base-URL
override pointed at a gateway that fronts them. The routes added no model
breadth, and because nothing exercised them they rotted quietly: Vertex Claude
was returning 404 for unversioned model ids and nobody noticed.

Stored provider configurations of those kinds are not migrated. A config row or
keychain credential written by an older build simply stops parsing and is
ignored.

What would bring them back: real demand for marketplace-billed access that a
gateway base URL cannot serve — an organization that must pay for inference
through its Google Cloud or AWS commitment and cannot put a compatible endpoint
in front of it. That case would justify the cloud-credential plumbing again,
and it would need a verified route and a live check, not a route curated on
documentation alone.

## Writing outputs back into a connected folder, headlessly

A headless install can read a folder an operator connected — list it, read text
out of it, import a file from it as a source. Publishing an output *into* one is
deliberately refused there, with a stable `output_writeback_authority_unavailable`
result rather than a second write path.

Writing into a user's folder is not just a host write. On the desktop it goes
through the exec write overlay, which snapshots the destination, routes a
replacement through the trash, and leaves the change reversible from the
conversation. A headless embedding installs no folder-grant resolver, so it has
none of that: no staged copy, no snapshot, no undo. Improvising one would be a
second, weaker way to overwrite a file the operator cannot take back, which is
worse than declining.

Lifting this means giving the headless engine the same overlay and write-back
machinery the desktop has, and a headless surface for the approval that a
replacement always requires — not relaxing the refusal.

## Additional desktop distribution

Production releases ship universal macOS packages plus x86_64 and ARM64
Windows and Linux packages. One distribution improvement remains deliberately
outside the cross-platform release contract:

- **Windows Authenticode signing.** The NSIS installer is signed by the Tauri
  updater key but not by a Windows-trusted code-signing certificate, so
  SmartScreen can warn on first run. Lifting this needs an accepted signing
  provider, protected release-environment credentials, verification of the
  signed installer, and a recovery procedure for certificate or provider
  failure.

Reliability work also remains ahead of the product surface: replayable adapter
contracts, recorded response decoding, and a protected live canary matrix would
catch provider API drift before it becomes a user-visible turn failure. MCP app
and gateway follow-ups are retained as a small, prioritized maintenance list:
prompt server replies during an HTTP stream, propagate theme changes into app
views, recover visibly from a transient frame-payload failure, make gateway
sign-in restartable, and validate the external-app protocol against its SDK.

## Code mode: what the first version deliberately leaves out

Code mode ([`docs/code-mode.md`](code-mode.md), decision records from 30
onward) ships
structured-first: harnesses are driven through their machine-readable
protocols, and the product's answer to "the protocol doesn't carry it" is a
visible capability gap, not a workaround. Several adjacent ideas are parked on
purpose:

- **Running a harness interactively in a PTY.** The escape-hatch version of
  code mode — when the structured protocol breaks, fall back to a terminal
  running the harness TUI — is rejected for now, not merely unbuilt
  ([record 36](decisions/0036-code-mode-auxiliary-terminals.md)). Its
  existence would sap the pressure to keep adapters honest, and it forfeits
  approvals, resume, and durable history. Reconsider only if a harness's
  machine-readable surface proves genuinely unusable over time.
- **A custom or system harness binary.** Every engine runs the bundled pin
  ([record 45](decisions/0045-run-code-mode-on-windows.md)). The pin shares
  HOME-scoped config and credentials with a system install, so vendor login,
  modelctl-managed gateway auth, and the hosted relay all work without
  pointing spawn at a different executable. Revisit when a machine has a
  real need to run a newer or custom binary than the pin.
- **Checkpoint restore.** Per-turn checkpoints land with v1 as hidden refs
  and power turn-scoped diffs; the surface that restores a workspace to an
  earlier checkpoint waits until review flows have settled.
- **Parallel turns in one worktree.** Several agents may share a workspace
  ([record 55](decisions/0055-multiple-sessions-per-workspace.md)), but their
  turns are serialized on the checkout: two harnesses editing one tree is
  corruption, not concurrency. Running turns side by side would mean a
  worktree per session, which splits the branch, the diff, and the pull
  request the workspace is keyed to. Reconsider only with a reason to give
  one workspace several trees.
- **Changing a session's permission mode underneath a running turn.**
  Changing the mode on a live session shipped in #2411. The session asks its
  engine on the engine's own channel first — Claude Code's
  `set_permission_mode` control request, Codex's per-turn policy fields — and
  only an engine that fixes its posture at launch pays for a stop and
  re-attach. The mode is still refused per harness capability
  ([record 33](decisions/0033-code-mode-approvals.md),
  [record 38](decisions/0038-auto-is-a-declared-capability.md),
  [record 39](decisions/0039-allow-is-a-first-class-code-permission-mode.md)),
  now at the change as well as at creation, and each change is journaled.
  What stays excluded is moving the posture while a turn runs: a turn that
  began under one mode must not have it changed underneath it, so the change
  is refused with `turn_running`. Reconsider only with a reason to let a
  running turn's approvals change shape mid-flight.
- **An in-app code editor beyond saving text files.** The file viewer now
  edits and saves one existing text file at a time, against the hash it
  loaded, so a save does not overwrite an agent's change nobody has seen
  ([code mode](code-mode.md#editing-files)). What stays deferred: creating,
  renaming, and deleting files; editing in a sandbox workspace, where the
  checkout lives in the sandbox; multi-file find and replace; and editing
  inside the diff panel. Each is a product surface of its own. Reconsider one
  when people ask for it.
- **Chat–code convergence.** The plan of record is
  [decision 48](decisions/0048-one-interaction-model.md): five independently
  useful steps, with code-mode structures as the merge survivor and chat
  contributing the content model. Steps 1 through 4 and the internal engine
  behind the adapter are in; the entity merge is under way. What stays
  deferred: the collapse of the chat and code pages into one surface
  (the record's 2026-09-01 amendment keeps two surfaces over one model),
  and whether the internal engine later becomes an external harness
  process.
- **A per-repo worktree-location override.** Worktrees live under one root per
  install — `~/Tidebreak/workspaces` on the desktop, the Tidebreak data
  directory for a headless deployment — and an operator can move that root
  ([record 53](decisions/0053-code-worktrees-live-in-a-user-visible-root.md)).
  What no repo can do is name its own location; toolchains that misbehave
  outside the repo's ancestry are the known cost, and the override waits for
  real instances of that pain.
- **A local relay for the mobile client.** The hosted mobile path shipped in
  epic [#2644](https://github.com/brightwave-inc/tidebreak/issues/2644), and
  the phone can now also attach straight to a standalone machine — a direct
  URL and a token from its roster, no gateway in the path
  ([record 98](decisions/0098-standalone-machine-attach-on-mobile.md), #3404).
  That covers a machine the phone can reach over trusted TLS. What remains
  deferred is the outbound machine link ("The laptop as the machine" in the
  Slack section below), which is what a phone would need to reach a
  *laptop* — a machine with no stable address and no certificate anyone
  issued.

## Tidebreak in Slack: what the first delivery leaves out

The Slack epic (#3178) closed on September 22, 2026 with the core product
deployed. A mention in a channel or DM starts a session, and no repository is
required
([record 94](decisions/0094-repository-optional-conversations-on-the-internal-engine.md)).
The session runs where the deployment can run it
([record 88](decisions/0088-a-slack-session-runs-where-the-deployment-can-run-it.md)).
Channel sessions act as one shared forge identity under a workspace grant
([records 89](decisions/0089-service-principals.md),
[90](decisions/0090-a-session-acts-as-one-forge-identity.md),
[92](decisions/0092-workspace-grants.md), and
[96](decisions/0096-slack-channels-share-the-instance-github-app-repository-access.md)).
Approvals, questions, and plans settle from Slack
([record 91](decisions/0091-approvals-are-answerable-where-the-person-is.md)).
Anyone on the deployment opens the session on the web and stays signed in
across a reload. A parent creates repository children, waits on them, and the
web and the thread show the tree. Attachments and images reach the session.
[`slack-sessions.md`](slack-sessions.md) records what is true. The tracks
below were scoped on that epic and are parked here, not abandoned. Each
closed issue keeps its acceptance criteria; reopen or refile one when it has
an owner.

- **A bounded children summary on the parent's digest** (was #3191). A
  parent now parks durably on the children it named and resumes with their
  ordered results, and `SessionSnapshot` carries the parent link. The digest
  still reports `parent_session` and wait counts rather than a bounded
  summary of the children themselves. Parent deletion already leaves children
  running (#3478).
- **Self-drive tools as one contract** (was #3192). The internal engine's
  native tools (`code_repos`, `code_session_create`, `code_run_turn`,
  `code_wait`, `code_sessions`) and the managed-sandbox bridge (record 94)
  are in. Not in: an MCP mount with a session-scoped `tbreak_hl_` token for
  ordinary machine harnesses, schema parity with `agent-mcp`, separate
  workspace creation, a placement override and budget inheritance on the
  call, and the stolen-token boundary test.
- **Research runs as children** (was #3193). `spawn_sandbox_run` returning
  a sandbox's `task_output` as a wait result, mixed child-session and run
  waits, and a typed missing-runtime conflict that offers a machine child
  instead.
- **Tree-aware budgets** (was #3194). Operator settings for the sandbox cap
  and spend ceilings shipped (`TIDEBREAK_RUNTIME_*`). A per-tree ceiling a
  child inherits from, spend rollup onto the parent, a machine-wide cap on
  harness children, and refusals that name the setting did not.
- **Multi-repository workspaces and their delivery** (was #3196, #3197). A
  workspace is one repository
  ([records 32](decisions/0032-code-workspaces-worktrees-checkpoints.md) and
  [53](decisions/0053-code-worktrees-live-in-a-user-visible-root.md)). Work
  across two repositories today is a parent with two children, each with
  its own pull request.
- **The delivery loop in the thread** (was #3203, #3204, #3205; gateway
  #1864). Pull-request facts, watch state, and turn cost on the external
  event stream; trigger turns on Slack-bound sessions; `fix`, `merge`,
  `rebase`, `ready`, and `close` as contribute-gated routes. The thread
  today ends at the pull-request link.
- **Participants and sharing on the session header** (was #3181).
  Authorized channel sessions are visible on the web and shared-workspace
  controls ship. The participants header, the owner's Share control for
  viewers and contributors by principal, and the viewer and contributor
  stories do not.
- **Files the session produces** (was #3200). Inbound images from a thread
  reach the session and were verified live. Files the session writes do not
  come back to the thread as Slack files or links.
- **Retention, deletion, and audit for Slack-originated content** (was
  #3201). A configurable retention window with a sweep, a per-session
  delete that also drops the adapter's rows, and an audit read listing every
  Slack-originated action with its actor.
- **The laptop as the machine** (was #3199, gateway #1859). An outbound
  WebSocket from the desktop's embedded server so a DM session runs on a
  laptop with no hosted machine and survives sleep.
- **Live acceptance without a sandbox runtime** (was on #3185). The
  internal-engine path has a regression test (#3445). The live channel
  drill on a machine with no runtime needs a standalone deployment, since
  production runtime settings are not weakened to run it.

## Memory that outlives a session

Decision records
[67](decisions/0067-memory-records-and-scopes.md) and
[68](decisions/0068-memory-backend-boundary.md) specify durable,
reviewable memory: scoped records captured from completed turns, reviewed
before they gain authority, and injected as a bounded digest into both
surfaces. The first slices are ordinary issues. What stays parked here:

- An organization-scoped shared store with deliberate promotion of
  personal records and review on the way up, behind the same backend
  boundary, once a governed remote store exists to host it. Promotion is
  explicit and record-by-record; nothing is ever derived from observed
  traffic.
- Cross-repository episodic search, recap retrieval, and integration with the
  curated memory tier. Per-workspace transcript search already exists.
- A continuous one-way mirror of memory records into a user-chosen
  folder, and a folder-backed storage backend for users who keep
  knowledge in versioned plain files. The mirror is derived output, never
  read back.
- Semantic retrieval as a declared backend capability, only with a
  readiness signal that distinguishes an unready index from an empty
  result, and retrieval diagnostics that make degradation visible.
- A dedicated memory model role, if maintenance judgment outgrows what
  the utility role's models deliver.

## What this means for planning

V1 is not a claim that Tidebreak has every kind of automation or connector. It
is a commitment to make the capabilities it does expose legible, local-first,
and bounded. New ideas should be added here when they describe a deliberate
product direction or a dependency outside this repository. Once a direction has
a concrete, buildable slice, turn that slice into a normal issue, claim it, and
remove it from this document when it ships or is reconsidered.
