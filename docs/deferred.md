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
- Enforce the strict `network off` setting in Docker code execution, then use
  the existing egress topology to make the other network policies enforceable.
- Make deletion of a terminal chat erase its background-run workspaces without
  waiting for the periodic reaper.

These are important finishing work, but they do not change the product's basic
shape. They should be delivered as normal, reviewable slices rather than held
in a parking label.

## A client for self-host teammates

The self-host profile already has named users and an admin/member split on the
API. What a teammate runs today is that API and the `tidebreak` CLI with a
static bearer token from the operator file. The packaged desktop app stays a
local Desktop-profile product: it embeds its own server and does not point at
a remote deployment.

A later member client can be a desktop connection mode (URL, token, TLS
expectations, Settings panels that degrade on a member `403`) or a hosted web
UI the deployment serves. Either one is a second product surface, not an
omission in the current server. Auth beyond the static token file waits on
gateway-derived identity.

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

## A browser without ambient computer control

Browser automation is a major missing capability, but it must not become a way
to take over the user's everyday browser or desktop. The future browser surface
needs its own Tidebreak-managed profile, clear cookie and login lifetime rules,
visible foreground control, durable history, cancellation, and a reset path.

The initial contract must distinguish navigation from entering sensitive text,
uploading files, downloading files, and submitting an external action. It must
also keep page-authored content separate from model instructions and from host
credentials and capabilities. Personal-profile access by default, CAPTCHA
evasion, and password-manager integration are not in this plan.

Desktop computer use — screen capture and consent-gated control of native
apps — is no longer parked; it is specified by
[decision record 13](decisions/0013-computer-use-screen-capture-and-app-control.md)
as its own capability with per-app grants. That record deliberately keeps the
everyday browser out of its starter scope: operating the user's browser
through the accessibility channel is not a substitute for the managed browser
surface described here, which stays deferred until its contract above is
real.

## More ways to organize and shape work

The basic chat remains useful on its own. Later work can make it easier to
organize and direct:

- Search inside an already approved connected folder from the composer, using
  root-relative paths and bounded discovery rather than exposing absolute host
  paths.
- Let a reusable plugin provide an output template as well as a prompt or
  skill, once there is a settled way to deliver a template into a turn.
- Install instruction-only plugins from pinned Git sources or public skill
  indexes, with explicit updates and no background auto-update.
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
the current path.

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

## Windows packaging

Windows builds shipped through v0.34.0 as an unsigned x86_64 NSIS installer,
and the code that produces them is still in the tree. The lanes are parked, not
deleted: `windows-check` in CI and the `prepare_windows`/`build_windows` jobs
in the release workflow are each gated behind a literal `false`, and the
`windows` descriptor in `scripts/create-release-manifests.mjs` is retained
outside `RELEASE_PLATFORMS`. Releases are macOS-only until those are flipped
back.

The reason is cost, not a product decision. The native Windows runner is the
slowest lane in the repo and the release build serializes a prepare and a build
job behind it, which dominates the time to publish a tag. Nothing about the
Windows boundary has been declared unsupported, and no Windows behavior has
been removed.

What resuming has to account for: the `/releases/latest/download/` link in the
docs site points at an installer that later releases will not carry, and
`latest.json` no longer publishes a `windows-x86_64` key. The in-app update
loop runs only on macOS today, so no live updater breaks — but a Windows user
on v0.34.0 has no upgrade path until the lanes come back.

Reliability work also remains ahead of the product surface: replayable adapter
contracts, recorded response decoding, and a protected live canary matrix would
catch provider API drift before it becomes a user-visible turn failure. MCP app
and gateway follow-ups are retained as a small, prioritized maintenance list:
prompt server replies during an HTTP stream, propagate theme changes into app
views, recover visibly from a transient frame-payload failure, make gateway
sign-in restartable, and validate the external-app protocol against its SDK.

## Code mode: what the first version deliberately leaves out

Code mode ([`docs/code-mode.md`](code-mode.md), decision records 30–36) ships
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
- **Bundled or pinned harness binaries.** Tidebreak resolves the user's own
  installed CLIs and never brokers their credentials
  ([record 34](decisions/0034-harness-discovery-credentials.md)). If version
  drift becomes a recurring support burden, the pinned-runtime install
  pattern already used for the managed Node runtime is the recorded escape
  hatch.
- **Checkpoint restore.** Per-turn checkpoints land with v1 as hidden refs
  and power turn-scoped diffs; the surface that restores a workspace to an
  earlier checkpoint waits until review flows have settled.
- **Multiple sessions per workspace.** The data model keeps room for
  follow-up and successor sessions in one worktree; v1 runs one active
  session per workspace.
- **An in-app code editor.** V1 reviews server-produced diffs and hands
  editing to the user's editor via the worktree path. An embedded editor is
  a heavyweight dependency with its own product surface; it needs demand
  evidence first.
- **Chat–code convergence.** The end state is one surface with no mode
  choice: one conversation concept with an optional workspace binding, where
  a workspace-bound conversation behaves code-like and engines — external
  harnesses and Tidebreak's internal loop alike — sit behind the adapter
  contract and are selected per conversation. The two modes are built
  shape-compatible so this stays a mechanical merge
  ([record 30](decisions/0030-code-mode-separate-surface.md)); the
  convergence itself is a future record on top of two proven models.
- **A per-repo worktree-location override.** Worktrees live under the
  Tidebreak data directory; toolchains that misbehave outside the repo's
  ancestry are the known cost, and the override waits for real instances of
  that pain.
- **Code mode on Windows.** Login-shell discovery and worktree pathing both
  need Windows-specific work, and Windows packaging is itself parked (see
  above).

## What this means for planning

V1 is not a claim that Tidebreak has every kind of automation or connector. It
is a commitment to make the capabilities it does expose legible, local-first,
and bounded. New ideas should be added here when they describe a deliberate
product direction or a dependency outside this repository. Once a direction has
a concrete, buildable slice, turn that slice into a normal issue, claim it, and
remove it from this document when it ships or is reconsidered.
