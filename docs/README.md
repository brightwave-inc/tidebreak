# OpenWave documentation

Project documentation, versioned alongside the code.

- [The crates](crates.md) — what each crate in the workspace is and does, and how
  they fit together.
- [How OpenWave works](how-openwave-works.md) — a plain-language maintainer tour
  of the product, runtime, state machines, document model, and unfinished edges.
- [Host access and connected folders](host-access.md) — how projects and
  conversations receive user-approved access to folders on the host machine.
- [Agent runs and sandboxed background work](agent-runs.md) — the shared
  foreground/background loop, depth-one agent hierarchy, durable waits, and
  bounded sandbox scheduling plan.
- [Tool architecture and roadmap](tools.md) — the current tool surface,
  foreground/sandbox split, provider boundaries, and web-search plan.
- [Web search configuration](web-search.md) — the local host-owned Exa/Tavily
  selection boundary and its current no-tool state.

More to come as the product surfaces land (running locally, API reference, and
writing tools).

For API-level docs, `cargo doc --open` renders the module documentation straight
from the source.
