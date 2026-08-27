# 79. A supervised agent declines the sandbox protocol

- Status: Accepted
- Date: 2026-08-27
- Owners: code mode, harness integration
- Related: [`0031-harness-adapter-boundary.md`](0031-harness-adapter-boundary.md),
  [`0015-tidebreak-product-and-technical-identity.md`](0015-tidebreak-product-and-technical-identity.md),
  [`0041-pinned-harness-binaries.md`](0041-pinned-harness-binaries.md),
  [`0044-install-pinned-harnesses-on-linux.md`](0044-install-pinned-harnesses-on-linux.md),
  [`0048-one-interaction-model.md`](0048-one-interaction-model.md),
  [`docs/deferred.md`](../deferred.md)

## Context

Two crates already define how an agent runs inside a sandbox Tidebreak
hosts. `tidebreak-sandbox-protocol` is the typed contract: an
exact-version attach handshake, a sequenced and resumable event cursor,
deny-by-default capability grants, a reverse-RPC channel whose first
capability is host-proxied model inference, and a conformance suite over
an in-process reference backend. Its `wire` module ships the concrete
byte transport — newline-delimited JSON framing (`serve_connection`,
`read_frame`) over a connection the host dials.
`tidebreak-sandbox-agent` is the workload that speaks it: the
host dials into the sandbox and attaches
(`tidebreak-sandbox-protocol::serve_connection`, the supervisor's TCP
listener in `crates/tidebreak-sandbox-agent/src/supervisor.rs`), and the
agent runs Tidebreak's own model loop, drawing inference inward through
the host so no model credential enters the container.

[`docs/deferred.md`](../deferred.md) sketches a third shape under "Remote
session execution": the same harness running in a managed sandbox, the
session ingesting a sequenced remote event stream into the same journal,
the workspace a remote clone whose results arrive as pushed branches. In
that shape an externally supervised sandbox — a controlled execution
environment that Tidebreak does not provision — starts the agent, owns
the durable event stream, and exposes the control endpoint. The agent
inside initiates outbound polls to that endpoint, drives an engine CLI
through `tidebreak-harness`, prepares the workspace and repository
clone, and reports lifecycle events outward.

Before that binary exists, two questions need definite answers so the
build does not relitigate them: does it reuse the sandbox-protocol
machinery, and what is it called?

## Decision

**The new binary declines `tidebreak-sandbox-protocol`.** It is a new
crate, `tidebreak-supervised-agent`, sharing engine mechanics through
`tidebreak-harness` and types through `tidebreak-core`, and nothing
through the sandbox protocol. Four properties of the protocol point the
wrong way:

- **Connection direction.** The protocol assumes the host dials the
  sandbox: `AttachRequest`, `serve_connection`, and the supervisor's
  `TcpListener` all exist so an outside host can attach to a listener
  the sandbox runs. The supervised agent runs no listener and accepts no
  attach — it initiates outbound polls to a control endpoint it does not
  define. There is nothing for the handshake to shake.
- **Cursor ownership.** The sequenced cursor, bounded event buffering,
  and acknowledgement-based retention exist so the journal-owning host
  can resume the sandbox's stream without loss. In the supervised shape
  the external endpoint is the durable side: it assigns sequence numbers
  and owns replay. Carrying a second cursor inside the agent would
  duplicate state the environment already guarantees.
- **Reverse RPC is dead weight.** The capability grants and the
  reverse-RPC channel exist because `tidebreak-sandbox-agent` runs
  Tidebreak's own model loop and must borrow the host's inference. The
  supervised agent drives an engine CLI whose inference route the
  environment configures; it makes no reverse calls, so the grant
  machinery would gate nothing.
- **The conformance suite certifies the wrong behaviors.** It exercises
  attach, replay, and idempotent operation identity — none of which the
  supervised agent performs. The wire framing the crate ships frames
  exactly that attach conversation: a stream the host dials into the
  sandbox. Reuse fails on direction, cursor ownership, and reverse RPC,
  not on a missing transport, and grafting HTTP polling underneath the
  suite buys nothing the control endpoint's own API does not already
  define.

**The name is `tidebreak-supervised-agent`.** `tidebreak-agent` says
nothing about which way the connection points, and that direction is the
entire distinction this record draws — a bare "agent" crate next to
`tidebreak-sandbox-agent` would make every reader look up which one is
which. `tidebreak-sandbox-agent` cannot stretch to cover this
shape, because its defining property is the opposite connection
direction — the host reaches into the sandbox and inference flows
inward, where the supervised agent reaches out and inference is the
engine's own affair. "Supervised" names what actually distinguishes the
new crate: an external supervisor owns the environment and the event
stream, and the agent reports to it.

**The crates coexist.** Neither subsumes nor deprecates the other.
`tidebreak-sandbox-protocol` and `tidebreak-sandbox-agent` remain the
in-flight attach contract: Tidebreak dials into a sandbox it supervises
and runs its own model loop there, a path
[`docs/deferred.md`](../deferred.md) keeps attached-only and opt-in
while the supported run stays in-process. `tidebreak-supervised-agent`
is the outbound shape, where an external environment provisions the
sandbox and the agent polls out. Only the supervised agent sits on
`tidebreak-harness`; the sandbox agent runs Tidebreak's own model loop
over reverse RPC, and whether that loop graduates into a harness-driven
process stays parked
([`0048-one-interaction-model.md`](0048-one-interaction-model.md)).

## Alternatives Considered

### Adopt the protocol wholesale

Rejected. Every load-bearing piece — handshake, cursor, grants, reverse
RPC — encodes the host-attaches-inward session shape. The supervised
agent would implement the interfaces and then leave them unreachable.

### Add an HTTP-polling backend to the protocol

Rejected. The crate's backend seam swaps byte transport under the same
typed session; it does not invert who connects to whom or who owns the
cursor. A "polling backend" would be a second protocol wearing the
first one's types.

### Extend `tidebreak-sandbox-agent` with a polling mode

Rejected. One binary with two opposite connection directions, two
inference postures, and two event-ownership models is two agents in a
trench coat. The shared substance — engine drive, event vocabulary —
already lives in `tidebreak-harness` and `tidebreak-core`, so a merged
crate would share only confusion.

### Name it `tidebreak-remote-agent`

Rejected. "Remote" describes where it runs, which is equally true of
`tidebreak-sandbox-agent`, and so distinguishes nothing. The
distinguishing fact is external supervision.

## Consequences

- The supervised agent's build has a settled dependency floor:
  `tidebreak-harness` and `tidebreak-core`, no sandbox-protocol
  dependency, no listener, no reverse RPC.
- Two agent crates must stay legible side by side. The rule of thumb the
  names encode: `sandbox-agent` is attached to, `supervised-agent`
  reports out.
- Protocol evolution stays cheap in both directions: the sandbox
  protocol can keep tightening its attach contract without auditing a
  polling consumer, and the supervised agent can track its control
  endpoint's API without a version dance against `PROTOCOL_VERSION`.
- What would reopen this: the externally supervised environment growing
  an attach-style inward channel, or the supervised agent needing
  host-proxied inference or capability grants. Either would make the
  protocol's machinery load-bearing rather than dead weight, and the
  reuse question should be re-asked then.
