# 37. Store methods that both real impls provide are required

- Status: Proposed
- Date: 2026-08-15
- Owners: persistence
- Related: [`crates/tidebreak-core/src/storage/store.rs`](../../crates/tidebreak-core/src/storage/store.rs)

## Context

`Store` is one object-safe trait behind `Arc<dyn Store>`. It has grown to
hundreds of methods. Most of those have a default body that returns
`Err("… is not implemented by this Store")`. `DbStore` implements nearly
every method. `MemStore` and the server test wrappers implement a small
subset and inherit the rest as that error.

The defaults were convenient: a 40-method test fake still type-checks as
`dyn Store`. They are also how the kitchen sink grew. Every new surface
could land as an optional method. `MemStore` and the wrappers silently
missed it. Production was the only honest impl. A second real store
would drown.

Nothing on the trait is unused. Every error-default method is overridden
by `DbStore`. Deleting methods is not available. The remaining choice is
how the trait is allowed to grow.

Eleven methods already have a real body on both `DbStore` and `MemStore`:
the document catalog and the root-attachment change surface. Their
error defaults are dead code that pretends those surfaces are optional.

## Decision

A `Store` method whose default is only `Err("… not implemented")` becomes
required the moment two impls in this repository provide a real body.
The eleven document and root-attachment methods become required now.

New persistence surface still lands on this trait (object safety is not
being reopened). It lands as a required method, or it does not land. An
error-default is not a third option. `MemStore` and the test wrappers
must grow with the surface they actually exercise, or they keep the
error only by writing it in their own impl.

Real defaults stay: owner-scoped delegates, compatibility projections
(`list_document_ids` mapping `list_documents`), and the
`*_and_append_event` unwraps. Those are behavior, not a missing impl.

Capability traits (`TurnStore`, `DocumentStore`, …) composed behind the
same `Arc<dyn Store>` are not chosen. They would split the kitchen sink,
and they would also split every atomic operation that today touches two
of those surfaces in one transaction. Revisit that if a second
production impl appears.

## Alternatives Considered

- **Do nothing.** The next six months of features keep adding optional
  methods. `MemStore` stays a 40-method object-safety proof that lies
  about every newer surface. Rejected because that is how the trait
  got to 260 methods.
- **Make every error-default required in this change.** That is ~186
  stubs on `MemStore` and a forwarder on every test wrapper for methods
  those tests never call. The compile break is real; the coverage is
  not. Rejected as a rewrite, not a growth rule.
- **Split `Store` into capability traits now.** Object-safe composition
  plus cross-surface transactions is a new boundary two subsystems
  would both be held to. No second production impl exists to force it.
- **Delete unused methods.** There are none. Every error-default is
  implemented by `DbStore`.

## Consequences

- Adding a persistence method is now a compile break on every `impl
  Store`. That is the point: the missing impl is visible in the same
  change as the method.
- Test wrappers that wrap `inner: Arc<dyn Store>` and do not forward
  must grow a forward (or a stub) when a newly-required method is
  something their tests actually invoke. Methods they never call can
  stay as an explicit `Err` in the wrapper.
- `MemStore` remains a partial store. It is no longer allowed to hide
  that behind a trait default for surfaces it already implements.

Revisit if a second production `Store` appears, or if a new surface
cannot be expressed without an atomic write that the single trait
cannot name.

## Validation

- `MemStore`, `DbStore`, `PauseTerminalStore`, and `TerminalFaultStore`
  compile with the eleven methods required.
- A new `impl Store` that omits `create_document` fails to compile.
- Owner-scoped delegates and other real defaults still compile without
  overrides on `MemStore`.
