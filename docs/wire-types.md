# Wire types

The desktop UI talks to the server over JSON produced by serde. Both sides had a
hand-written description of that JSON and nothing connected them, so the two
could disagree while every test on both sides passed. Two shipped defects came
from exactly that, both optionality mismatches invisible to either suite.

The TypeScript is now generated from the Rust definitions.

## Running it

```sh
# Rewrite the generated bindings after changing a wire type.
UPDATE_WIRE_TYPES=1 cargo test -p tidebreak-server
```

Output is checked in under `crates/tidebreak-desktop/ui/src/generated/`. Without
the environment variable the same test *compares* instead of writing, and fails
if the checked-in file is stale — so CI fails on a diff.

**The check is the guarantee, not the generation.** Regenerating by hand is fine.
Shipping a stale file is what fails.

## Why generate rather than diff a hand-written file

`ts-rs` reads the same serde attributes serde does, so `tag`, `rename_all`,
`skip`, and `skip_serializing_if` are honored by construction rather than
mirrored by hand. The alternatives were weighed against the surface as it
actually is:

- **`typeshare` is ruled out.** It has no `flatten` support, and `McpServerInfo`
  uses it.
- **OpenAPI is a poor fit.** The renderer event stream is a WebSocket, not REST,
  so an OpenAPI round-trip would leave the event projection — the largest closed
  boundary here — ungoverned. It also loses the most fidelity on precisely the
  serde attributes the two shipped bugs came from.

## What is deliberately not generated

- **The runtime validators** in `api.ts` (`parsePendingToolApproval` and
  friends). They are a trust boundary, not a shape check: they enforce character
  bounds, reject control characters, refuse duplicate ids, pin one server string
  to a frozen constant, and re-derive the server's own policy booleans to check
  they agree. Generated types sit underneath them as the narrowing target.
- **The eight `Option<Option<T>>` PATCH bodies.** Absent, `null`, and a value are
  three distinct states, and no generator expresses the difference — `ts-rs`
  renders it as `T | null | null`, which collapses to `T | null` and loses
  "absent". These are all *inbound* request bodies; nothing outbound uses the
  pattern, so the generated response half is unaffected.
- **`ModelSelectionKey`**, a template-literal refinement of a plain Rust
  `String`.

## Adding a type

1. Derive `TS` on the Rust type. If it is reachable from a type already
   generated, that is all — the generator walks the dependency closure from a
   small set of roots, so a new field of a new type pulls it in automatically.
2. If a field is `#[serde(skip_serializing_if = "…")]`, add `#[ts(optional)]`.
   Serde omits the key, but `ts-rs` only infers optionality from
   `maybe_omitted && has_default`, and serde treats a missing `Option` as `None`
   without needing `#[serde(default)]` — so without the annotation you get
   `field: T | null`, claiming a key the server never sends. **This is the exact
   mismatch that shipped twice, and generating the type does not fix it.** A test
   scans for it and fails with the field name, so you do not have to remember.
3. Regenerate, and read the diff. A change here is a change to what the server
   promises.

## What CI checks, and which mistake each check catches

Every one of these is a class of mistake that previously had to be caught by
reading the code. They all run in existing lanes; nothing needs a new job.

| Check | Catches |
|---|---|
| The generated file matches the Rust types | Any wire change that was not regenerated |
| A field serde omits is declared optional | `skip_serializing_if` rendering as `T \| null`, the mismatch that shipped twice |
| Precision-critical fields generate as unions | A field loosened to a string, silently dropping an allowlist the renderer's tables are keyed on |
| No `any`, `bigint`, or `unknown` in the output | A type reaching the roots that cannot be expressed, or an integer rendered as `bigint` |
| Validators run against generated fixtures | A field renamed server-side under a hand-written validator — the case where both suites stayed green |
| Ids generate as bare strings | `#[serde(transparent)]` being ignored in a way that stops coinciding with the right answer |
| The journal event shape is pinned | A rename in a persisted type, which stops existing chats loading |

Several of those are backstops rather than the primary defence, and it is worth
knowing which, so nobody weakens the real guard thinking a test still covers it.
The strongest protection is **not deriving `TS` on types that must not reach the
renderer**: `serde_json::Value` and the stored four-variant `Role` both fail to
compile if a generated type tries to carry them, which no assertion can match.
The tests catch the cases that would compile — a new field, or a type swapped
along with its call sites.

The fixture check is the one worth understanding, because it closes the failure
this document opens with. The validators are a trust boundary and stay
hand-written, so generation cannot check them — but their *tests* used to build
their own inputs, which encoded what the author believed the wire looked like. A
field renamed server-side left both suites green and the app broken. Those tests
now consume `generated/fixtures.ts`, serialized from real server values, so a
rename changes the fixture and the renderer test fails.

Rejection cases stay hand-authored: malformed input is not something the server
can produce, so there is nothing to generate from.

## Two things the generator gets wrong by default

Both are configured or annotated, and both are pinned by tests, but they are
worth knowing before adding types.

- **Large integers.** `ts-rs` renders `i64`/`u64` as `bigint`. These types
  describe what `JSON.parse` produces, and that is a `number` — a `bigint`
  declaration would be false about every value received, and would break
  arithmetic against existing `number` state. The generator sets
  `large_int` to `number`.
- **`#[serde(transparent)]`.** `ts-rs` cannot parse it and ignores it. Every id
  newtype carries it, and the right output happens anyway because a single-field
  tuple struct already renders as its inner type. That coincidence is pinned by a
  test, and the resulting per-id build warning is silenced by the
  `no-serde-warnings` feature.

## Scope today

**Generated: the whole renderer surface.** The WebSocket frame and event union, the
tool vocabulary and previews, approval kinds, the transcript and its source records,
all three consent surfaces, and the configuration, catalog, project, chat, and
agent-run DTOs.

**Hand-written, deliberately.** Nine declarations, each for a reason:

| Type | Why |
|---|---|
| `PendingToolApproval`, `PendingFolderAccessRequest`, `PendingUserQuestions`, `UserQuestion`, `UserQuestionOption` | camelCase app types describing the *validator's output*, not the wire. Their wire counterparts are generated and imported as `Wire*`, and each validator's key allowlist is tied to `keyof` that type. |
| `ToolResultPreview` | Same, and its Rust type is carried in the journal — renaming those fields would stop existing chats loading. |
| `ServerInfo` | Tauri IPC, a different contract from REST. |
| `ApprovalGrantRung`, `UserQuestionAnswer` | Inbound request bodies. |
| `ModelSelectionKey` | A template-literal brand over a plain Rust `String`; no generator expresses it. |

Two generated types carry a visible override rather than being aliased, both
written with `Omit` so the divergence is legible:

- **`ChatMessage.citations`** stays optional. Despite the internal field name,
  these records populate the desktop's **Sources** row. The server always sends
  it, but the transcript arrives as a parsed cast with no validation, so the `?` is what
  forces the guard that reads it. Narrowing it would delete that guard rather
  than earn it.
- **`ModelInfo.key`** is re-branded as `ModelSelectionKey`. The wire is honestly
  a string; the brand is the app-level refinement that keeps a
  provider-qualified key distinct from a bare model id.

`ChatMessageSnapshot.citations` is deliberately wider in TypeScript than on the
wire: the server always sends it, but the transcript is not validated, so the `?`
is what forces the guard that reads it. Narrowing it would delete that guard
rather than earn it.
