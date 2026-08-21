# Maintainer documentation

This directory explains how Tidebreak is built, which contracts the code must
preserve, how releases and deployments work, and which product directions are
deliberately parked. It is versioned with the implementation and written for
contributors and operators.

User-facing guides live in [`docs-site/content/docs/`](../docs-site/content/docs)
and publish to [tidebreak.io/docs](https://www.tidebreak.io/docs/). The product
overview and launch copy belong to the separate
[`brightwave-inc/tidebreak-site`](https://github.com/brightwave-inc/tidebreak-site)
repository. Keep those surfaces focused rather than copying the same product
explanation everywhere.

Some pages below describe a current contract; others describe a staged or
parked design. A design page should say its status near the top. Numbered
decision records are the authority when later implementation choices depend on
an accepted boundary.

## Start here

- [How Tidebreak works](how-tidebreak-works.md) — end-to-end tour of startup,
  turns, tools, files, durability, clients, and the main reliability rules.
- [The Tidebreak crates](crates.md) — workspace dependency direction and the
  responsibility of each crate.
- [Decision records](decisions) — accepted boundaries, rejected alternatives,
  and the conditions under which a decision should be revisited.

## Agent runtime and product contracts

- [Foreground agent operating prompt](agent-operating-prompt.md) — capability
  composition, trust boundaries, diagnostics, and extension rules.
- [Agent runs and background work](agent-runs.md) — execution hierarchy,
  delegation, durable waits, scheduling, and supervision.
- [Durable user questions](user-questions.md) — structured clarification and
  exact wait/resume behavior.
- [Conversation outputs](deliverables.md) — output publication, versioning,
  restore, delete, and background-agent artifacts.
- [Host access and connected folders](host-access.md) — the brokered boundary
  between a conversation and user-approved host files.
- [Folder bindings for local apps](folder-bindings.md) — app-specific folder
  consent, fingerprints, and dispatch enforcement.
- [Local apps](local-apps.md) — generated app records, sandboxed rendering,
  invocation consent, and publishing ownership.

## Tools, execution, and integrations

- [Tool architecture](tools.md) — built-in tool registration, renderer
  vocabulary, foreground and sandbox surfaces, and reliability rules.
- [Code execution](code-execution.md) — the provider-neutral `exec` contract,
  network policy, workspaces, local backends, and managed backends.
- [Execution providers and sandbox-resident runs](sandbox-providers.md) — the
  parked design for detached runs, credential separation, admission, and the
  sandbox-agent protocol.
- [Web search](web-search.md) — search backends, page extraction, source
  publication, and the local configuration boundary.
- [Connected apps](connected-apps.md) — shared records and authorization for
  external integrations.
- [External MCP servers](mcp-servers.md) — desktop and headless configuration,
  health, refresh, approvals, and MCP App views.
- [Tested and community MCP servers](mcp-tested-servers.md) — what a curated
  listing claims and how entries are maintained.
- [Model providers and cross-provider replay](model-providers.md) — provider
  tiers, switching routes mid-conversation, and flatten-on-switch.
- [Code mode](code-mode.md) — the second product surface: repos,
  worktree workspaces, and structured sessions over external coding-agent
  harnesses.

## Interfaces, deployment, and operations

- [Diagnostics](diagnostics.md) — local snapshots, OpenMetrics, structured
  events, export bundles, and privacy limits.
- [Wire types](wire-types.md) — generating the desktop TypeScript contract from
  Rust serde types and checking it in CI.
- [Self-hosting](self-hosting.md) — deployment profile, token roles, Compose,
  reverse proxies, backup, and upgrades.
- [Tidebreak ↔ model gateway boundary](gateway-boundary.md) — managed-profile
  pairing, authentication, policy tiers, and wire responsibilities.
- [OS-managed policy](managed-policy.md) — macOS, Windows, and Linux policy
  artifacts plus the developer toggle.
- [Releases and versioning](releases.md) — semantic PR titles, release
  automation, packaging, staging, and the path to 1.0.

## Planning

- [What comes after v1](deferred.md) — canonical home for intentionally parked
  product scope and the conditions that would bring it forward.

For API-level documentation, run `cargo doc --open`.
