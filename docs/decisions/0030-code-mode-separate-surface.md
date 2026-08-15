# 30. Code Mode Is a Separate Surface Built for Later Convergence

- Status: Proposed
- Date: 2026-08-15
- Owners: code mode
- Related: [`0007-cli-headless-feature-parity.md`](0007-cli-headless-feature-parity.md),
  [`0002-pre-v1-schema-and-persisted-format-mutability.md`](0002-pre-v1-schema-and-persisted-format-mutability.md),
  [`docs/code-mode.md`](../code-mode.md)

## Context

Tidebreak's existing product is a conversation-first coworker: a chat owns a
private scratch workspace, the foreground agent loop drives a configured model
provider, and every capability (documents, outputs, folders, approvals) is
shaped around that loop. The workspace path is deliberately never shown to the
user; host files reach execution only through brokered folder grants.

Coding work inverts several of those assumptions. Developers already run
dedicated coding-agent CLIs — Claude Code, Codex CLI, Grok CLI, opencode — that
own their own agent loops, tool sets, permission systems, and billing
identities. What they lack is an interface: parallel sessions are a pile of
terminal tabs, nothing distinguishes "waiting on you" from "working" from
"done an hour ago", and reviewing what an agent actually changed means leaving
the tool. Tidebreak wants to be that interface: pick a repository, spin up
isolated sessions, and supervise them through a structured UI — conversation,
tool activity, approvals, diffs, and a pull-request flow — rather than through
raw terminals.

The forces:

- A coding session's engine is an external process with a foreign lifecycle,
  not Tidebreak's own agent loop. Its workspace is a real git worktree the
  user must be able to open, build in, and eventually merge — the opposite of
  a hidden scratch directory.
- The chat data model is deeply coupled to the internal loop: turns assume
  provider steps, approvals assume the internal tool registry, documents and
  outputs assume conversation-owned files. Overloading those types would force
  every chat feature to answer "what does this mean for code?" from day one.
- The two products should nevertheless become one coherent coworker over time.
  A user who delegates a spreadsheet and a refactor should eventually get one
  inbox, one attention surface, and one way to organize work. Divergence that
  makes that future harder is a defect even while the modes are separate.
- [`0007`](0007-cli-headless-feature-parity.md) establishes that the server
  API is the product surface. A second product surface does not change that
  rule.

## Decision

Tidebreak gains a second mode, **Code**, as a separate product surface whose
internals are deliberately shaped for later convergence with chat.

**Separate now:**

- **Its own route family.** The desktop UI serves code mode under `/code/...`
  with its own sidebar; the chat surface is unchanged. A persistent rail
  affordance switches modes.
- **Its own vocabulary.** The nouns are **repo**, **workspace**, **session**,
  and **turn**:
  - A *repo* is a user-registered local git repository: root path, default
    base ref, branch prefix, setup and archive scripts, quick actions.
  - A *workspace* is one isolated unit of work on a repo. It owns exactly one
    git worktree and one branch for its whole life, and carries the
    pull-request state for that branch.
  - A *session* is one durable conversation with one external coding harness,
    running inside a workspace. V1 permits one active session per workspace;
    the model keeps room for follow-up and successor sessions.
  - A *turn* is one user→agent cycle within a session, ending in a
    checkpoint.
  - The word *harness* names the external CLI being driven. It is not called
    a provider — providers are model routes; a harness is a whole agent.
  "Project" is deliberately not used: that noun already names the chat
  product's project entity and must not acquire a second meaning.
- **Its own tables and journal.** Code-mode state persists in new tables with
  new id spaces. No chat table gains a code-mode column, no code table
  references a `ChatId` or `TurnId`, and code sessions never appear in chat
  lists or routes. The code journal is a separate table.
- **Server-API-first.** Every code-mode capability lands as a
  `tidebreak-server` route or WebSocket channel; the desktop UI and the CLI
  are clients. Native-only concerns (a directory picker for repo
  registration) follow the existing closed Tauri command allowlist rules.

**Convergent by design.** Wherever the two modes need the same *kind* of
thing, code mode adopts the chat mode's existing shape rather than inventing a
sibling, so that a future unification is a mechanical merge instead of a
translation layer:

- The code journal uses the same event-table shape, sequencing rule,
  journal-before-live-publication discipline, and WebSocket
  snapshot→replay→live contract as the chat journal. A future unified journal
  is a table merge, not a protocol negotiation.
- The permission vocabulary reuses the chat product's mode names and meanings
  (`Plan`, `Ask`, and an automatic tier) rather than a harness-flavored
  synonym set.
- The attention model introduced for code sessions
  ([`0035`](0035-code-mode-wire-contract.md)) is defined over "a unit of
  supervised work", not over code sessions specifically, so chat can adopt it
  later without renaming.
- Inbox-shaped items (pending approvals, fenced sessions, review-ready turns)
  are modeled so they can appear in the existing cross-chat Inbox when the
  modes align; in v1 they surface only in code-mode UI, but the wire shape is
  not code-private in ways that would block that.
- UI building blocks are shared components (tool cards, approval cards,
  markdown rendering, panel system, sidebar primitives), not forks.
- Id types are distinct (so the separation is enforceable) but structurally
  identical to chat ids, and all wire types ride the same generated-types
  pipeline.

Gratuitous divergence — a different streaming idiom, a different settings
mechanism, a private design language — is out of bounds without a decision
record explaining why the shared shape cannot serve.

**The destination is one surface, and the engine slot is the primary
abstraction.** The mode split is a delivery strategy, not the intended end
state. The product Tidebreak is steering toward has no user-facing mode
choice at all: a conversation bound to a repo-backed workspace behaves
code-like, and a conversation without one is ordinary chat — context selects
behavior. In that end state the adapter contract of
[`0031`](0031-harness-adapter-boundary.md) is the product's runtime
interface, and Tidebreak's internal agent loop holds no privileged seat: it
is one engine implementation among several, selected where its capabilities
(document work, brokered host access, outputs) are the honest best fit, with
the same capability-flag honesty rules that govern external harnesses.
Design choices in either mode should be tested against that destination:
a feature that assumes the internal loop is special, or that the two
surfaces are permanent, is going the wrong way.

Deliberately excluded from this record: the unification itself (one inbox,
one conversation concept with optional workspace binding, engines selected
per conversation). That is a future record built on two proven models; this
record's job is to keep it cheap.

## Alternatives Considered

**Extend the chat model now.** Add a session kind to chats and reuse messages,
turns, and approvals. Rejected: a chat turn is an internal provider loop with
provider steps and registry-bound tool calls; a code turn is a foreign process
emitting a foreign protocol. Every shared invariant would need a carve-out,
and each mode's changes would put the other in their blast radius exactly
while code mode is at its most experimental.

**Full separation with no convergence constraints.** Let code mode pick
whatever shapes are locally convenient. Rejected: the modes are one product to
the user, and every locally convenient divergence (a second streaming idiom, a
second permission vocabulary) becomes a translation layer in the unification
we already intend.

**A separate application.** Rejected: duplicates the server, keychain,
settings, design system, packaging, and update channel for no isolation
benefit the route/table separation does not already provide.

**Root the vocabulary in "projects".** Rejected: guaranteed collision with the
existing project entity in code, wire types, and UI copy; code mode's natural
root object is a git repository.

**Do nothing** (leave coding workflows in terminals outside Tidebreak).
Rejected: supervising parallel coding agents is exactly the
attention-and-review problem Tidebreak's structured surfaces exist for.

## Consequences

Two products share one shell. Some duplication is accepted knowingly — code
mode gets its own approval records, its own event enum, its own sidebar
sections — but each duplicate is required to be shape-compatible with its chat
counterpart, which constrains code-mode design freedom on purpose.

The separation is load-bearing for safety: chat-mode invariants (brokered host
access, hidden workspace paths) are not weakened by code mode's opposite
choices (user-visible worktrees), because the two never share a data path.

New tables are a baseline schema change and advance the desktop schema epoch
per [`0002`](0002-pre-v1-schema-and-persisted-format-mutability.md).

Revisit this decision when both models are proven and the unification can
begin: one conversation concept with an optional workspace binding, engines
selected per conversation behind the adapter contract, and no user-facing
mode choice. The convergence constraints above are the measure of whether
this record did its job: unification should require merging structures, not
translating them.

## Validation

- A repository check (test over the entity definitions) that no chat entity
  references a code-mode table or id type and no code-mode entity references
  a chat table or id type.
- Route tests that `/code/*` routes reject chat ids and chat routes reject
  code-mode ids rather than resolving them.
- A UI test that the code-mode route family renders without initializing chat
  session stores, and vice versa.
- Convergence checks: the code events WebSocket passes the same
  replay/reconnect test shape as the chat events route; the code permission
  enum's variants are a subset of names shared with the chat vocabulary; code
  inbox-shaped wire types carry no field that hard-codes a code-only id as
  their display identity.
- A plausible wrong implementation would pass all chat tests while quietly
  foreign-keying code sessions into the chat `event` table; the entity check
  above must fail it.
