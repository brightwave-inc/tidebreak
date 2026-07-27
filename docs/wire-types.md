# Wire types

The desktop UI talks to the server over JSON produced by serde. Both sides had a
hand-written description of that JSON and nothing connected them, so the two
could disagree while every test on both sides passed. Two shipped defects came
from exactly that, both optionality mismatches invisible to either suite.

The TypeScript is now generated from the Rust definitions.

## Running it

```sh
# Rewrite the generated bindings after changing a wire type.
UPDATE_WIRE_TYPES=1 cargo test -p openwave-server
```

Output is checked in under `crates/openwave-desktop/ui/src/generated/`. Without
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

1. Derive `TS` on the Rust type.
2. If a field is `#[serde(skip_serializing_if = "Option::is_none")]`, add
   `#[ts(optional)]`. Serde omits the key; without the annotation `ts-rs` emits
   `field: T | null`, which claims the key is always present. This is the exact
   mismatch that shipped twice, and it is the one thing the generator will not
   catch for you.
3. Regenerate, and check the diff reads the way you expect.

## Scope today

Generation currently covers the renderer's tool vocabulary. The remaining wire
types — the snapshot DTOs and the event projection — are still hand-written; see
the tracking issue. Two known mismatches are already documented there rather than
silently corrected: `CustomModelConfig.display_name` is `skip_serializing_if` but
typed as required in TypeScript, and `ChatMessageSnapshot.citations` is always
serialized but typed optional.
