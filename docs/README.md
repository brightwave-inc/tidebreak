# Tidebreak documentation

Project documentation, versioned alongside the code.

- [The crates](crates.md) — what each crate in the workspace is and does, and how
  they fit together.
- [How Tidebreak works](how-tidebreak-works.md) — a plain-language maintainer tour
  of the product, runtime, state machines, document model, and unfinished edges.
- [Model providers and cross-provider replay](model-providers.md) — provider
  tiers, mid-conversation switching, and the flatten-on-switch rule.
- [Host access and connected folders](host-access.md) — how projects and
  conversations receive user-approved access to folders on the host machine.
- [Agent runs and sandboxed background work](agent-runs.md) — the shared
  foreground/background loop, depth-one agent hierarchy, durable waits, and
  bounded sandbox scheduling plan.
- [Foreground agent operating prompt](agent-operating-prompt.md) — deterministic
  capability composition, trust boundaries, versioning, and extension rules.
- [Durable user questions](user-questions.md) — foreground-only structured
  clarification with exact wait/resume behavior across reload and restart.
- [Tool architecture and roadmap](tools.md) — the current tool surface,
  foreground/sandbox split, provider boundaries, and web-search plan.
- [Wire types](wire-types.md) — generating the desktop's TypeScript from the Rust
  serde definitions, and what is deliberately still hand-written.
- [Web search configuration](web-search.md) — the local host-owned Exa/Tavily
  selection boundary and its current no-tool state.
- [Code execution](code-execution.md) — the provider-neutral `exec` contract,
  native local sandbox, configuration boundary, and managed-provider extension
  path.
- [Execution providers and sandbox-resident agent runs](sandbox-providers.md) —
  the run-tier/execution-provider split, reachability and credential
  invariants, and the phased route to detached background agent runs.
- [Connected apps](connected-apps.md) — the umbrella record for outside
  integrations: MCP servers as one kind, the planned REST kind with its
  governed local executor, and the promotion-compatible binding vocabulary.
- [Local apps](local-apps.md) — agent-generated mini-apps in the profile:
  sandboxed frame rendering, manifest-pinned tool invocation with durable
  consent, and how sharing is registered here but published at the gateway.
- [The Tidebreak ↔ model gateway boundary](gateway-boundary.md) — how a profile
  becomes gateway-managed (pairing, policy tiers, sessions) and what crosses
  the wire once it is.
- [OS-managed policy (MDM)](managed-policy.md) — the per-platform artifacts an
  administrator deploys to point Tidebreak at a managed model gateway.
- [Releases and versioning](releases.md) — semantic PR titles, native release
  drafts, tag-derived macOS builds, and the deliberate path to `1.0.0`.
- [What comes after v1](deferred.md) — the deliberately parked product scope,
  the v1 finishing work that remains actionable, and the conditions that bring
  future capabilities forward.
- [Decision records](decisions) — numbered records of decisions later work has
  to live with, each stating what was chosen, what was rejected, and what would
  cause it to be revisited.

More to come as the product surfaces land (running locally, API reference, and
writing tools).

For API-level docs, `cargo doc --open` renders the module documentation straight
from the source.
