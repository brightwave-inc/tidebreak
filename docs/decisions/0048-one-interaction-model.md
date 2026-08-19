# 48. One interaction model for chat and code

- Status: Proposed
- Date: 2026-08-19
- Owners: server, code mode
- Related: [`0030-code-mode-separate-surface.md`](0030-code-mode-separate-surface.md),
  [`0031-harness-adapter-boundary.md`](0031-harness-adapter-boundary.md),
  [`0033-code-mode-approvals.md`](0033-code-mode-approvals.md),
  [`0035-code-mode-wire-contract.md`](0035-code-mode-wire-contract.md),
  [`0038-auto-is-a-declared-capability.md`](0038-auto-is-a-declared-capability.md),
  [`0039-allow-is-a-first-class-code-permission-mode.md`](0039-allow-is-a-first-class-code-permission-mode.md),
  [`0046-rich-turn-input-rides-harness-capabilities.md`](0046-rich-turn-input-rides-harness-capabilities.md),
  [`0047-gateway-linked-hosting.md`](0047-gateway-linked-hosting.md),
  [`docs/deferred.md`](../deferred.md) ("Chat–code convergence")
- Supersedes: none

## Context

Decision 30 built code mode as a deliberately separate model and named its
own destination: "no user-facing mode choice at all: a conversation bound to
a repo-backed workspace behaves code-like, and a conversation without one is
ordinary chat — context selects behavior," with the internal agent loop
holding "no privileged seat" behind the decision-31 adapter contract. It also
set the criterion — unification must mean "merging structures, not
translating them" — and told us to revisit "when both models are proven."
`docs/deferred.md` parks the same item as "a future record on top of two
proven models." This is that record.

Both models are now proven enough that the divergences are concrete rather
than hypothetical. What exists twice today:

1. Two permission enums with identical variants (`PermissionMode` and
   `CodePermissionMode`, both `Plan | Ask | Auto | Allow`), differing in
   lifecycle: chat's is changeable per chat, code's is fixed at session
   creation and refused per declared harness capability (decisions 38, 39).
2. Two journals implementing the same snapshot-replay-live contract: the
   `event` table with `/chats/{id}/events`, and `code_event` with
   `/code/sessions/{id}/events` plus the unsequenced `/code/updates` channel
   (decision 35).
3. Two approval models: chat's grant ladders and approval judge; code's
   verbatim-payload rows with deny-with-feedback and deliberately no
   standing grants (decision 33).
4. Two turn state machines: chat's with durable waiting-for-client
   continuations; code's with spawn-epoch fencing and pid-probe recovery
   against a foreign process.
5. Two attention surfaces: the chat inbox keyed on `ChatId`; code's
   server-computed `AttentionState`, which decision 30 already notes is "not
   code-private in ways that would block" a shared surface.
6. Two ownership postures: chat, document, and app rows are owner-scoped;
   `code_*` rows carry no owner at all.
7. Two id spaces, enforced by repository checks so neither side can
   reference the other.

What raised the price of divergence: gateway-linked hosting (decision 47)
means every client surface — the desktop app attached to a remote machine,
then web and mobile clients — must otherwise speak two wire contracts, two
journals, and two attention systems, forever, on every platform.

## Decision

Convergence becomes the plan of record, as a sequence in which each step is
independently useful and lands before the next begins. The decision-30
criterion governs throughout: every step merges a structure or does not
happen; translation layers and discriminator columns remain rejected
(decision 35's reasoning stands until the entities themselves merge).

The merge has a direction, and it is not monolithic per side. **Code mode's
structures survive**: the session and turn shape, spawn-epoch fencing, the
sequenced per-session journal with the cheap updates digest channel
(decision 35), the attention model, and the verbatim approval rows — these
are the general case, built for supervising an engine the server does not
control, and they are what remote and mobile clients need. **Chat
contributes the content model**: attachments, rich turn input, documents and
deliverables, and its grant ladders — re-expressed as capabilities of the
internal engine rather than as a parallel subsystem. Decision 46's
"attachment model" direction and this direction compose: chat's content
riding code's structure. In the end state a chat is a session with the
internal engine and no workspace, not a second kind of thing.

1. **Owner scoping for code.** `code_*` tables gain owners and `/code/*`
   joins the owner-scoped regime. Required by decision 47 regardless of
   convergence; it goes first because it is paid once.
2. **One permission vocabulary.** A single `PermissionMode` type, with
   code's lifecycle as the default: the mode is chosen at conversation
   creation, and mid-conversation changes are a capability an engine
   declares (the internal engine declares it; harnesses mostly do not)
   under the decision-38/39 honesty rules, instead of two parallel enums.
3. **One attention surface.** The inbox adopts the code attention model:
   every conversation, chat included, carries an `AttentionState`
   (decision 30 already notes the wire shape is not code-private), so a
   supervising client — the phone case — watches one queue with one
   vocabulary. This forces the shared conversation identifier that step 5
   needs, and is the first place the id spaces meet.
4. **One turn-submission contract.** Decision 46's own revisit clause
   executes: code turn input merges into the chat attachment model rather
   than diverging further, with capability gates deciding what an engine
   accepts.
5. **One conversation, engines behind the adapter.** The internal agent
   loop implements the decision-31 adapter contract as one engine among
   several. A conversation bound to a repo-backed workspace selects a
   harness engine; one without selects the internal engine. Chat and code
   entities merge here into the code-shaped structures — turns, journal,
   approvals — and the repository checks that kept the id spaces apart are
   retired in the same change that makes them one space. The hard part is
   named up front: chat's turn machinery — durable waiting-for-client
   continuations, queued turns, the user-questions cards — must become
   expressible through the adapter contract, because nothing may reach the
   internal engine through a side door the adapter does not have. Whether
   the internal engine then runs in-process behind the adapter trait or
   graduates into an external harness process of its own is deliberately
   deferred; the adapter contract is the boundary either way, and this
   record does not choose the packaging.

Approvals merge structurally in step 5 but keep their semantic split as
capability, not subsystem: external harnesses retain verbatim-payload,
no-standing-grants behavior (decision 33) as a declared property of the
engine, while the internal engine keeps its grant ladders. One approval
surface, two honesty postures.

New client surfaces beyond the existing desktop app target the converged
contract; the desktop client may bridge both contracts in the interim.
Steps 1 through 4 are pre-convergence and safe to land while decision 47's
remote wire is being proven; step 5 starts only after it is, so the merge
happens on a contract that real remote clients have exercised.

Deliberately excluded: a big-bang schema merge; reusing the chat `event`
table for code via a discriminator (rejected in decision 35, still
rejected); removing the harness adapters or re-privileging the internal
loop; and any change to what host authority a machine has (decision 47's
territory).

## Alternatives Considered

- **Keep two models indefinitely.** Every client pays twice on every
  platform, and the wire contract for remote clients doubles. Decision 30
  never intended this; rejected.
- **Translate instead of merge.** A compatibility layer mapping code
  entities into chat shapes at the route layer. This is exactly the outcome
  decision 30 pre-rejected ("merging structures, not translating them") —
  it fossilizes both models under a third. Rejected.
- **Converge first, host second.** Cleanest wire for remote clients, but it
  delays hosting behind the largest refactor in the codebase, and hosting is
  what teaches which contract survives contact with remote clients.
  Rejected as ordering; the sequence above interleaves them instead.
- **Merge everything except the journals.** The journals are the wire; a
  merge that leaves two event streams leaves clients speaking two
  protocols, which is the cost this record exists to remove. Rejected.
- **Do nothing until 1.0.** The 1.0 compatibility commitment would then
  freeze two parallel contracts. Converging before 1.0 is the whole value
  of being pre-1.0. Rejected.

## Consequences

- Steps are sized to be individually shippable, but step 5 is still a large
  change to turns, journal, and approvals at once; the pre-1.0 disposable
  regimes (desktop epoch, and decision 47's narrowed posture for
  gateway-linked deployments) are what make it affordable. Landing step 5
  after 1.0 would cost migrations this plan never budgets for.
- Until step 5, clients carry both contracts. Web and mobile clients
  started before step 5 completes would inherit that double cost, which is
  an argument for sequencing them after it — decision 47 defers to this
  record on that point.
- The permission and attention merges (steps 2 and 3) change persisted
  shapes and wire payloads; each takes the standard pre-1.0 baseline edit
  plus epoch bump, stated per decision 2.
- Capability honesty becomes the single mechanism carrying every behavioral
  difference between engines. If a difference cannot be expressed as a
  declared capability, that is the signal it is a design problem, not a
  flag.
- Revisit if step 5 shows chat's continuation model genuinely cannot be
  expressed through the adapter contract (it, not code's fencing model, is
  the side that must fit) — the fallback is one journal and one surface
  over two execution backends, which is still most of the value. Revisit
  also when the deferred packaging question is answered: if the internal
  engine becomes an external harness process, that is its own record on
  top of this one, not an edit to it.

## Validation

- The wrong implementation of step 5 keeps two tables and translates at the
  route layer. The check is structural: after the merge, no route handler
  maps one side's entities into the other side's shapes, and the retired
  id-space repository checks are replaced by tests that one id resolves in
  one space.
- A conversation with no workspace must behave as today's chat: the
  existing journal shape fixture, retargeted per decision 2, is the tripwire
  that the merge changed structure deliberately rather than accidentally.
- After step 2, an engine that declares a fixed permission mode still
  refuses mid-conversation changes — the honesty rules, not the removed
  enum, must carry that behavior.
- After step 3, a code approval surfaces in the shared inbox and is
  answerable from a renderer-shaped client with no code-mode-only routes
  involved.
- Owner scoping (step 1) validates as in decision 47: no cross-owner rows,
  and no cross-owner events on the updates channel.
