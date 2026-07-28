# Execution providers and sandbox-resident agent runs

This document defines where OpenWave runs background agent work once it stops
being a process on the local machine, what an isolated workload may call back
into, and the sequence from today's in-process scheduler to an agent loop that
executes inside an isolation boundary.

The execution-provider tier that already exists (local, E2B, Daytona) is
documented in [Code execution](code-execution.md); the durable run machinery
in [agent runs](agent-runs.md); the broker and capability model this design
reuses in [host access](host-access.md). This document is the design that
connects them.

## Tiers and vocabulary

"Sandbox" currently names two unrelated mechanisms.

| Tier | Today | What isolates the work |
| --- | --- | --- |
| Background agent run | `openwave-server` sandbox agent-run worker | Capability restriction: no history, fixed tool budget, bounded I/O. |
| Execution provider | `openwave-code-execution` | OS or VM enforcement: Seatbelt on supported hosts, a vendor sandbox remotely. |

These are orthogonal. Background agent work currently runs inside the OpenWave
server process; an execution provider performs one bounded process invocation
per call. A background agent run performs no code execution today, and an
`exec` call is issued by the foreground coordinator, not by a background
agent.

Vocabulary rule for new code and docs: background agent run for the run tier,
execution provider for the isolation tier; do not use bare "sandbox" as a tier
name. Existing model-facing and persisted identifiers (`spawn_sandbox_agent`,
`AgentRunExecution`) are not renamed by this rule. Two further terms defined
in [attachment and admission](#attachment-and-admission) — attached versus
unattached, and detached admission versus attached-only — carry most of the
weight in this document.

The persisted schema has one field where two meanings will collide.
`AgentRunExecution` is `foreground | sandbox`: its variants are documented by
who advances the run (the coordinator or the sandbox scheduler), while
[agent runs](agent-runs.md) describes the field as execution location. Both
readings agree today only because every run executes in-process. Once a
background run can execute either in-process or inside a provider boundary,
one field would have to carry the scheduler axis and the location axis at
once, so it splits into two before a second location exists. This is a
persisted-value and generated-wire-type change (see
[wire types](wire-types.md) and the pre-1.0 notes in [releases](releases.md));
sandbox-resident runs add row fields later regardless, but keeping the two
axes from fusing is what this early slice buys.

## Target state

The [tools](tools.md) roadmap lists possible later additions — a scratchpad,
pinned context, plan/execution modes, clipboard operations, generated images,
richer deliverables, and connected apps. Two of them — plan/execution modes
and connected apps — imply a background agent that outlives a single bounded
model call and works near credentials. The end state this document plans for
is a background agent run that executes inside an execution provider's
boundary — a container or managed sandbox — rather than inside the OpenWave
server process, with the host retaining ownership of admission, results,
credentials, and policy.

Two properties motivate it.

Containment. In the foreground loop, the process that parses model output and
dispatches tool calls is the same process that holds the keychain and the
broker control channel. Today's background worker is deliberately narrow — a
fixed budget of tools whose destinations and effects are host-fixed, even
where their payloads are model-authored — precisely because widening it
in-process would hand injected instructions host authority. Moving the loop
inside a provider boundary is what makes a wider tool surface (ultimately,
model-authored execution) admissible at all: model output can then only move
the sandbox, and the host consumes a bounded event stream and result as
untrusted input.

Containment has limits, stated here once. A child's result is still delivered
into the foreground agent's context, so adversarial text can ride the result
upward; the [operating prompt](agent-operating-prompt.md) already treats tool
and document content as untrusted data, and sandbox results and events join
that category. And a compromised in-sandbox agent can still drive whatever
authenticated egress its run was granted — a confused deputy the credential
holder will faithfully serve. Containment relocates authority; it does not
make hostile content safe.

Bounded detachment. A host-driven loop stops the moment the laptop sleeps,
the app quits, or the process dies. A sandbox-resident loop admitted for
detached execution (see [attachment and admission](#attachment-and-admission))
can keep working through host absence, within a window that is the minimum of
four bounds: the run's absolute deadline, the provider's sandbox lifetime,
the lifetime of the scoped model credential described below, and the
sandbox's event-buffer capacity. This is not "runs forever unattended"; it is
"survives the host being away for a bounded window, then reconciles" — which
is why detached admission is an explicit, gated mode rather than a default.
[Agent runs](agent-runs.md) defers detached agents; the
[sandbox-resident run semantics](#sandbox-resident-run-semantics) section
defines the semantics that lift that deferral when the relevant step lands.

The entry condition for moving the loop inside the boundary: a
sandbox-resident loop becomes mandatory before a background agent gains any
tool whose effects are model-authored rather than host-bounded — execution
above all — and before OpenWave promises work that survives host absence.
Below that line, the current worker's fixed budget is sufficient and cheaper.
The gate is mechanical, not prose: the background tool surface is a closed
set pinned by a test, in the same spirit as the generated renderer tool
vocabulary in [tools](tools.md), so widening it is a deliberate, reviewed
change to a named invariant rather than a drive-by registration. When the
loop relocates, the closed set and its test move with it: a sandbox-resident
run gets its own pinned tool registry, and widening that registry is a
separate design gated on this document.

## Design invariants

These hold across every phase and every provider.

1. The model never selects a provider, endpoint, timeout, or credential.
   Provider choice is host configuration, resolved at the last boundary
   before dispatch, exactly as code execution does today.
2. The host dials the sandbox; the sandbox never dials the host. In the
   local desktop deployment the server is a loopback listener on a machine
   that sleeps, moves networks, and has no public address, so nothing may
   assume the host is addressable; other deployment tiers are
   [host access](host-access.md)'s subject and inherit the same direction
   rule.
3. No credential of any kind enters the agent process. The host's own
   long-lived model-provider keys never enter the sandbox in any phase;
   detached operation uses short-lived, scoped tokens the host obtains and
   delivers at admission. Third-party credentials enter only the sandbox
   supervisor, the non-agent process defined in
   [reachability](#reachability-and-the-callback-channel) — see
   [credential separation](#credential-separation).
4. Host-authority operations require live consent. Anything that crosses the
   durable approval boundary — connected-folder access, deliverable
   acceptance — needs the host attached, so detached admission is refused
   for work that can reach such an operation; an attached-only run that
   reaches one while its host is briefly gone checkpoints in the sandbox
   and waits.
5. Hard outer bounds are enforced from outside the workload wherever the
   backend can express them, and detached admission requires them. While
   attached, the host enforces them — and because the host is the model
   proxy for attached-only runs, an attached-only run that loses its host
   starts no new model step; an already-dispatched tool call runs to
   completion under supervisor-enforced tool bounds, which are defense in
   depth, not a boundary. While unattached, the external bounds are the
   provider's sandbox lifetime — set at provisioning to no more than the
   run's absolute deadline wherever the backend supports a lifetime cap —
   and the scoped token's lifetime; supervisor-carried budgets and
   deadlines are again defense in depth, defeatable by a workload that
   escalates inside its own container. A backend with no lifetime cap is
   admissible attached-only, and its orphan case is bounded only by
   in-sandbox self-termination — a stated limit of those topologies, not a
   boundary.
6. Local stays the default and, on a supported host, stays fully functional
   with no vendor account and no third-party credential. Sandbox-resident
   capability is additive: an install without a container runtime keeps
   today's entire surface and simply does not offer the new mode.
7. The host's store remains the source of truth at every reconciliation
   point. While a run is unattached the sandbox holds not-yet-collected
   events; they become durable only when the host commits them, and results
   and events are committed exactly once through the fencing rules below.

## Reachability and the callback channel

Because the host dials out, the transport model is uniform across local
containers, managed vendors, and self-hosted backends: the host holds a
connection to the sandbox — request/response plus a resumable, monotonically
sequenced event stream — polls or streams events, and downloads artifacts.

The sandbox side of that connection is the sandbox supervisor, a process
distinct from the agent: the same non-agent component that holds credentials
(see [credential separation](#credential-separation)). The host mints a
per-run transport secret at provisioning time and delivers it through the
provider's control plane; the supervisor requires it on every request. The
secret is a bearer credential and a deduplication aid, not proof of sandbox
authenticity: whoever carries it — on a managed provider, the vendor's
control plane — can impersonate the run to the host, which is a
[trust model](#trust-model) entry, not fine print. The host pins what it can
per topology (TLS identity for a vendor-fronted endpoint, loopback address
plus the per-run secret for a local container), and everything arriving over
the channel — events, results, reverse-RPC requests — is treated as
untrusted input attributed to the run's provider. An unauthenticated
agent-control listener is not acceptable in any topology; OpenWave's own
local API is bearer-authenticated and the broker authorizes every operation
on a private channel, and this surface meets the same bar.

Some capabilities need the reverse direction — the in-sandbox loop asking
the host for a model completion while the host is the model proxy, host-side
search, a consent prompt. The plan is reverse RPC over the host-held
connection: the host keeps a bidirectional stream open and the sandbox
supervisor multiplexes requests back over it, authorized by capability
grants and audited, following the `openwave-host-broker` envelope pattern
(versioned protocol, deny-by-default capabilities, per-operation
authorization, bounded results). Two rules carry over from that pattern
unchanged: every reverse-RPC request bears a durable per-run operation
identity, and the host commits its effect and response keyed by that
identity, so a request re-issued after a reconnect receives the recorded
response instead of a second execution — a consent prompt is answered once
no matter how many times the connection drops. Consent prompts that arrive
this way are rendered with the run's provenance — which run, which provider
— so the user knows who is asking.

Reverse RPC is the highest-risk unproven element of this design. It needs
request correlation, cancellation, backpressure, and the idempotency rule
above — a real protocol — and none of it exists yet. It gets a dedicated
spike before any step depends on it, and model inference is the first
capability that spike must carry, because a sandbox-resident loop cannot
take a single step without it. The spike also fixes the disconnect semantics
the host must observe: an in-flight reverse-RPC request whose connection
drops is failed to the sandbox side, and re-issue is safe by the idempotency
rule.

If the spike fails, the consequence is stated plainly: sandbox-resident runs
then require a scoped-token issuer even while attached, so an installation
whose only model credential is a long-lived provider key cannot run them at
all, and host-mediated capabilities (search, consent prompts) are absent
from sandbox-resident runs until a callback channel exists. That is a hard
product regression, which is why the spike precedes the protocol step rather
than following it.

## Attachment and admission

Two independent axes govern a sandbox-resident run, and conflating them is
how designs in this space go wrong:

- Attachment is a live fact: the host currently holds the run's connection
  (attached) or does not (unattached). It changes with host presence, not
  with anyone's intent.
- Admission mode is a durable decision: whether the run may keep working
  while unattached (detached admission) or must not (attached-only, the
  default).

Every sandbox-resident run survives disconnection by checkpointing; only a
detached-admitted run keeps working through it. An attached-only run that
loses its host — the laptop sleeps mid-run — starts no new model step (the
host is its model proxy), lets any already-dispatched tool call finish under
supervisor-enforced bounds, checkpoints, and waits. Its absolute deadline
keeps running; if the host returns first, the run resumes; if the deadline
or the sandbox's lifetime expires first, reconciliation records the terminal
failure and tears the sandbox down. Where the backend supports a lifetime
cap, provisioning sets it to no more than the run's deadline, so an orphaned
sandbox dies on its own; where it does not (a local container, a self-hosted
backend whose destroy is a no-op), an orphan outlives a host that never
returns until the supervisor's own deadline fires — self-termination inside
the user's own infrastructure, stated as the limit it is. No admission
decision is revisited by a disconnect, and reverse-RPC availability is keyed
to attachment, never to admission mode.

## Sandbox-resident run semantics

Every sandbox-resident run — attached-only or detached-admitted — shares
one state model, and detached admission adds explicit preconditions on top
of it rather than a hope that leases stretch.

Preconditions. Detached admission is available only when all of the
following hold; otherwise admission fails closed and the run may still be
admitted attached-only:

- the model credential is a short-lived, scoped, revocable token from the
  user's model gateway or a provider that issues them. Long-lived provider
  API keys are never eligible, so a configuration whose only credential is
  a long-lived key cannot serve detached runs. A detached-admitted run uses
  this token for all its model traffic, attached or not — one path, so the
  window math below means what it says and the gateway sees the run's whole
  model history;
- the backend enforces a sandbox lifetime cap from outside the sandbox, set
  at provisioning to no more than the run's absolute deadline. A local
  container runtime has no such cap, so local runs are attached-only —
  detachment is what managed backends are for;
- the agent image is verified within the topology's trust root (see
  [trust model](#trust-model));
- the run's tool surface cannot reach a host-authority operation
  (invariant 4): work that needs consent mid-run is refused detached
  admission at the start, not parked indefinitely in the middle;
- if the run carries third-party credentials, egress policy is enforced from
  outside the sandbox by a mechanism the host knows out-of-band (see
  [egress policy](#egress-policy)).

The scoped model token is deliberately exempt from that last rule: a
detached, credential-free run on an open-egress provider could exfiltrate
its own token, and the design accepts that exposure because the token is
short-lived, endpoint-scoped, revocable by the gateway, and worth at most
bounded model spend — unlike a connected-app credential, whose blast radius
is the user's account. The token's model endpoint is recorded in the run's
egress allowlist, but on a supervisor-only enforcement tier that entry is
policy, not a boundary.

One attempt. Every sandbox-resident run — attached-only or detached — has
exactly one execution attempt; the run tier's multi-attempt retry machinery
applies only to in-process execution, and a sandbox-resident run is never
re-claimed into a second attempt. A host crash does not start one either:
recovery reconciles the existing sandbox, which is what checkpointing and
reattachment are for. External effects (model spend, connected-app calls)
cannot be proven unexecuted after a loss, so a lost or expired run fails
terminally and visibly — never re-executed, matching the repository-wide
rule that ambiguous execution is failed conservatively rather than
replayed. Retry, where the user wants one, is a new run admitted
explicitly. Run re-execution and sandbox teardown are different things:
teardown is idempotent and is always driven to completion (below), while
execution is never retried.

Admission and provisioning. The host commits a durable provisioning intent —
carrying a host-minted correlation tag — before asking the provider for a
sandbox, and stamps the tag into the provider's sandbox metadata at
creation, the same pattern the shipped managed `exec` adapters use for
workspace correlation. The returned handle is committed onto the run row
with the result idempotency key, all scoped to this single attempt, and
only after the handle commits does the host deliver the task, token, and
policy snapshot — a sandbox reclaimed before that point never executed
anything. The intent records its provisioning window, and recovery is
driven by the intent, not by what the provider reports: when the window
lapses with no committed handle, the admission fails on the intent —
whether or not a create ever reached the provider — and an orphan sweep
lists the provider's sandboxes by tag and destroys any that belong to a
lapsed intent, so a crash on either side of the create call converges on
the same terminal state and the sweep cannot race a slow in-flight create.
A handle learned for a run that is already terminal is enqueued for
teardown rather than ignored. Execution is fenced by the attempt itself:
exactly one sandbox was ever asked to run it, so the host never needs to
prove remote exclusion — only to decide, once, what became of that
attempt. Durable transitions for the run are serialized through the store
transactionally, the same discipline the run tier uses today; provider
network calls happen outside any lock, and correctness comes from the
durable intents and the commit predicate, not from mutual exclusion around
I/O.

While unattached. The sandbox supervisor carries the absolute deadline and
the loop budgets — defense in depth per invariant 5; the external bounds
are the provider lifetime cap and the token lifetime — buffers events under
a fixed size bound, and on overflow checkpoints and stops producing rather
than dropping events, a state reattachment resumes, not a terminal one. If
the scoped token expires before reattachment, the run checkpoints the same
way.

Reattachment. A returning host first drains: it resumes the event stream
from its last committed cursor and commits what the sandbox held — events
carry the sandbox's monotonic sequence numbers, the host commits a batch and
advances its cursor in one transaction, and re-delivered sequence numbers
are discarded, so a crash between read and commit re-reads without
duplicating and never skips. Only after draining does the host evaluate
deadline expiry, so work that finished inside its deadline is not discarded
by a late-waking reaper. Reverse-RPC capabilities return with attachment.

Result commit. A result commits if and only if it carries this attempt's
idempotency key and the run has no terminal state — one predicate, checked
in the committing transaction. A re-delivery of an already-committed result
is acknowledged with the original receipt, the same recovery the run tier
gives an ambiguous retry today. A well-formed result that carries the
attempt key but fails the predicate for any other reason — it arrived after
a cancellation, or after a deadline-expiry commit the drain rule could not
prevent — is never committed as the run's outcome, but is retained
attached to the terminal record as non-authoritative evidence: it is the
only testimony about the attempt's external effects that will ever exist,
and discarding it would erase exactly what one-attempt exists to respect.

Teardown. Every terminal transition of a sandbox-resident run — completed,
failed, cancelled, deadline exceeded — durably enqueues a teardown
obligation for the run's sandbox handle in the same transaction. A sweep
drives obligations to completion: destroy is idempotent from the host's
side, each attempt is journaled before the call, and an ambiguous or failed
destroy leaves the obligation open for the next sweep rather than being
abandoned. The run's terminal state never waits on teardown; a sandbox that
outlives its run is a bounded credential exposure (scoped token, snapshotted
policy) that the obligation and the provider lifetime cap exist to end. This
also covers the reaper: deadline expiry on an unattached run is decided
host-side against the store clock after the drain rule above, and the same
transition that records the failure enqueues the teardown that stops a
still-live sandbox from working on a run the host has already closed.

Cancellation. Cancelling a sandbox-resident run commits the terminal
`cancelled` state and enqueues the teardown obligation in one transaction,
like every other terminal path; the destroy outcome is recorded on the
obligation, never awaited by the terminal commit, so an unreachable provider
cannot wedge a run in a non-terminal state or hold a scheduler slot. (The
run tier's `cancelling` handshake lets a live lease-holding in-process
worker acknowledge; sandbox-resident execution holds no worker lease — a
persisted-shape change that lands with the sandbox-resident step — and the
result-commit predicate is what fences a straggler, so there is nothing to
hand-shake with.) While attached, the host may signal the supervisor to stop
gracefully before committing, but the commit is what cancels. A parent
cancellation cascades to a sandbox-resident child by the same one-transaction
rule per child, so the cascade cannot hang on a provider call.

## Credential separation

The invariant for third-party (connected-app) credentials: a third-party
credential never enters the agent's address space.

The mechanism is process separation inside the sandbox. The sandbox
supervisor — the same non-agent process that owns the transport listener —
holds credentials and fronts egress. The agent addresses a connected service
with an opaque placeholder; the supervisor substitutes the real credential
at its own boundary and strips agent-supplied authentication. The
placeholder is ergonomics; the separation is the guarantee, and a credential
holder inside the agent process provides no separation at all.

Stated honestly, what this does and does not give:

- It protects credential bytes from an agent compromised at its own
  privilege level. In-container separation (process, and UID where the
  platform allows — a requirement on the image, not something the local
  adapter demonstrates today) is weaker than a VM boundary; privilege
  escalation inside the container defeats it. Defense in depth, not a vault.
- On a managed provider, the credential exists inside vendor infrastructure
  while the run is live. Choosing a managed provider for credential-bearing
  work is a trust decision about that vendor — see the
  [trust model](#trust-model).
- Substitution requires TLS termination at the supervisor. A client that
  pins certificates fails closed rather than degrading. Documented limit.
- A compromised agent can still use the proxied credential for its granted
  destinations (the confused deputy above). Scoped grants and egress policy
  bound the blast radius; nothing eliminates it.

The credential path for background work is designed around this indirection
from the start, even though the supervisor ships late in the sequence —
retrofitting the inversion after a host-holds-everything path ships is much
harder. Connected apps inside sandbox-resident runs are blocked on the
supervisor existing; there is no interim mode where the agent holds a real
credential.

While attached the host remains the model proxy for attached-only runs —
the request that crosses the execution seam today is already credential-free
— and detached-admitted runs use the scoped gateway token described above,
held by the supervisor, never the agent.

## Egress policy

A small, dependency-free decision layer answers one question — may this
workload open a connection to this destination? — and every enforcement
point consults it: the local adapter (which denies network outright today),
the sandbox supervisor, and provider-level controls where a vendor exposes
them. Allowlists are user-granted, auditable, and revocable; the grant and
consent surface follows the broker capability model in
[host access](host-access.md), and tool-level approval stays with the
durable `Sensitive` boundary in [tools](tools.md) — egress grants do not
replace tool approvals or vice versa.

Two honesty rules keep this from becoming a label:

- Enforcement is tiered and the tier is stated. Policy enforced from
  outside the sandbox — the local adapter's network denial, a firewall the
  host configures around a local container, a vendor's per-sandbox network
  policy — is a boundary. Supervisor-enforced policy shares a failure
  domain with the workload and is defense in depth.
- Third-party-credential-bearing sandbox-resident runs require the external
  tier, and whether a backend has it is host knowledge, not a wire claim: a
  capability the host itself establishes (the boundary it configures around
  a local container) or one compiled into an adapter the host ships for a
  managed vendor — never a declaration a backend makes about itself, so a
  self-hosted backend is not eligible for third-party credentials on its
  own say-so. Both current managed adapters permit open internet access
  inside the sandbox (disclosed in settings), so until a vendor exposes
  per-sandbox egress enforcement, those providers run credential-free
  workloads only.

For an unattached run, policy is the snapshot delivered at admission.
Revoking a grant while its run is unattached takes effect at reattachment
or next admission, not instantly — revocation semantics follow host
presence, and the settings surface says so.

## The agent wire protocol

The protocol at issue here is the sandbox-agent boundary — provisioning,
init, the event stream, artifact collection, reverse RPC — not the `exec`
adapters that shipped in the execution-provider tier, which speak each
vendor's own API and are unaffected. A self-hosted sandbox backend means
someone other than OpenWave runs the sandbox side of this boundary, so the
wire contract is a public, versioned interface that third parties implement:
the protocol is the deliverable, and OpenWave's own backends are its first
consumers.

- Define the protocol before the backends that implement it. A protocol
  retrofitted onto shipped implementations inherits their accidents as
  permanent contract. (The workspace capability in the delivery sequence is
  host-internal adapter machinery and carries no such commitment; the
  protocol's artifact surface is defined fresh at the protocol step, scoped
  to the run.)
- Version it explicitly: a protocol version the host checks and refuses on
  mismatch, following `openwave-host-broker`'s exact-equality check. The
  protocol is declared unstable until a named release; while OpenWave is
  pre-1.0 nobody should build on it without expecting breakage.
- Split provisioning from addressing. The backend abstraction decomposes
  into provision (returns a handle; may be a no-op wrapping a user-supplied
  endpoint), address (handle to a reachable base URL), and destroy (may be a
  no-op). The self-hosted backend — no provisioning, just an address and a
  credential — is the conformance test: if it needs a special case, the
  abstraction is wrong.

Reachability of a self-hosted backend is the user's responsibility: the
endpoint must be dialable from the host by the user's own means (LAN, VPN,
or public ingress). OpenWave operates no rendezvous or relay, so a backend
behind NAT without one of those is simply not addressable — stated up front
rather than discovered.

## Delivery sequence

The implementation is intentionally incremental:

1. Split `AgentRunExecution` into run tier and execution location, and land
   the vocabulary rule. No product behavior change; it is a persisted-value
   and generated-wire-type change — existing `sandbox` rows map to
   `(background, in-process)` and `foreground` rows to
   `(foreground, in-process)` — with the read-model guards and the schema
   shape constraints re-expressed over two fields.
2. Execution providers behind the provider-neutral `exec` contract: local,
   E2B, Daytona. *(Shipped.)*
3. The egress decision layer, its capability-grant wiring, and the
   enforcement-tier surface — including the per-backend external-enforcement
   capability that the egress rule checks at admission.
4. Workspace lifecycle: an optional host-internal capability beside
   `execute` — create/connect/destroy, put/get/list files — for backends
   with a durable session. Local implements it over private chat scratch;
   managed providers over their session APIs; capability-flagged so callers
   degrade instead of failing. Workspace artifacts are proposals until the
   host accepts them into the conversation's output record, and acceptance
   is a host-side operation under invariant 4. The
   [conversation outputs](deliverables.md) contract currently ships bounded
   UTF-8 text with binary formats explicitly deferred; extending it to
   binary artifacts — including how a revision records a producing run
   rather than a turn — is in this step's scope and updates that document
   with it.
5. The reverse-RPC spike: transport, authentication, correlation, the
   operation-identity idempotency rule, cancellation, backpressure,
   disconnect semantics, with model inference as the first carried
   capability. Outcome is a go/no-go that decides whether attached
   sandbox-resident runs proxy inference through the host or the design
   falls back as stated in the reachability section.
6. The agent wire protocol: the versioned sandbox-agent boundary above, the
   provision/address/destroy decomposition, and the conformance suite —
   shipped in this step against an in-process reference backend, and run in
   CI against the local container backend from the next step onward. This
   step exists whether or not the spike says go; its reverse-RPC surface is
   shaped by the spike's outcome.
7. Sandbox-resident agent runs, in order:
   1. Local container first, attached-only — no vendor account, the
      development loop for everything after, and the reference a
      self-hoster reads. The container runtime is a detected capability (as
      Seatbelt is today) that reports unavailable when absent, never a hard
      dependency. The host-restart-while-the-container-lives case is
      exercised here as unplanned-disconnect recovery: checkpoint, no new
      model step, reconcile on return.
   2. Managed providers and detached admission second. The detached-run
      preconditions — gateway token, external lifetime cap, verified image,
      no host-authority tools, the egress rule — bind wherever detached
      admission is offered; local containers never qualify, because nothing
      outside the container bounds their lifetime.
   3. Self-hosted backends become expressible once the protocol is
      published; what remains is conformance, not new machinery.

   Exit criteria for this step exercise the semantics this document adds,
   not the ones the run tier already tests: unplanned disconnect and
   reattachment of an attached-only run; a host crash recovering by
   reconciling the existing sandbox rather than starting a second attempt;
   duplicate event delivery across reattachment discarded by sequence
   number; reattachment draining buffered results before deadline
   evaluation; a late result retained as evidence but not committed; a
   provisioning intent lapsing with no committed handle failing the
   admission and reclaiming any tagged sandbox; crash during destroy
   recovering from the journaled attempt without re-executing the run;
   crash after result commit with the teardown obligation still driven to
   completion; deadline expiry against a live unattached sandbox enqueueing
   teardown; event-buffer overflow checkpointing and resuming; a reverse-RPC
   request re-issued after reconnect receiving its recorded response; a
   parent cancellation cascading over an unattached child. Vendor-only
   behaviors (provider TTL expiry, control-plane ambiguity) are simulated at
   the protocol seam the same way the managed `exec` adapters are tested
   against injected HTTP today.

The in-sandbox agent image (loop plus sandbox supervisor) is built in step
7.1 and is a prerequisite of 7.2; connected apps inside sandbox-resident
runs are blocked on it. OpenWave's agent loop, tool trait, and provider
abstraction are already plain Rust in the core crates; running the loop
inside a sandbox is the same code with a different tool registry and
transport, not a second implementation. The sequence itself ships no change
to the model-facing tool surface: widening the sandbox-resident registry is
a separate design, gated on the entry condition above.

## Trust model

Self-hosted sandbox backends. Pointing OpenWave at a self-hosted backend
places that endpoint inside the trust boundary: it executes background work,
and with a callback channel it can ask things of the host. Attaching one is
a trust decision of the same weight as connecting a folder — explicit
consent, scoped capability grants, audit — not a URL field in settings. A
hostile backend must be assumed and contained by the same capability model;
its event stream and results are untrusted input to the host, and no
self-declared capability of the backend (egress enforcement above all)
unlocks anything.

Managed vendors. A managed provider sees everything inside the sandbox:
task content, workspace files, any credential delivered for a
credential-bearing run, and the per-run transport secret its control plane
carries. Sandbox authenticity is therefore rooted in the vendor for this
topology: a malicious control plane can impersonate the run — forge events,
results, and reverse-RPC traffic — and this design does not defend against
that; it makes the exposure visible (run provenance on consent prompts,
per-workload provider choice) and bounds it (the egress rule, short-lived
scoped tokens). Selecting a vendor is consenting to that position, and the
settings surface says so.

The model gateway. Detached runs depend on a token issuer that can mint
short-lived scoped credentials and revoke them while the host sleeps. The
gateway is a trusted party for detached operation: it observes detached
model traffic and it is the only actor that can cut a detached run's model
access off early.

The image and its supply chain. Every in-sandbox mechanism — the
supervisor, separation, budgets, deadline carriage — is only as trustworthy
as the image running it, which makes the image's publisher and distribution
path part of the trust boundary. Verification means different things per
topology, and the difference is stated rather than blurred: a local runtime
verifies a digest directly; a managed provider is asked to run a pinned
digest and trusted to do so — image integrity there is a vendor assertion,
in the same trust class as everything else the vendor controls. Where a
provider cannot even express digest pinning, detached admission and
credential-bearing runs are unavailable on that provider — attached-only,
credential-free work is the ceiling, and the settings surface says why.

## Testing

Consistent with the repository's testing policy, the CI artifact for the
protocol is one conformance suite, shipped with the protocol step against an
in-process reference backend and run against the local container backend
once it exists, including the semantics cases listed in the delivery
sequence. The managed adapters keep their existing CI coverage through
injected HTTP seams; what stays out of CI is live vendor exercise, which
needs paid accounts and credentials, is unrunnable on forks, and flakes.
Live verification is out-of-band with repository secrets. Bounding coverage
this way is stated here rather than left silent.

## Deliberate non-goals

- No sandbox-initiated connection to the host, and no installation-wide
  static secret anywhere in the design; the callback channel is
  capability-gated reverse RPC over the host-held, per-run-authenticated
  connection, or nothing.
- No inherited environments: tool subprocesses inside a sandbox get a
  cleared environment (the local adapter's existing hygiene) and, as an
  image requirement, run under a different UID than the agent process where
  the platform allows.
- No OpenWave-operated relay or rendezvous as a requirement of this design;
  every topology works with a purely local control plane. (Deployment tiers
  themselves are [host access](host-access.md)'s subject.)
- No weakening of consent: the foreground `exec` tool stays `Sensitive`
  regardless of provider strength, and a sandbox-resident run's execution
  capability is granted explicitly at admission — scoped to that run,
  recorded durably — never inherited or implied.
- No re-execution of ambiguous remote work, ever: a run that cannot be
  accounted for fails visibly; it is not resumed, guessed at, or replayed.
  (Teardown of its sandbox is retried; the work is not.)
- No Kubernetes-shaped control plane. Orchestrating pods, warm pools, and
  cluster networking belongs to whoever operates a self-hosted backend,
  behind the protocol.
