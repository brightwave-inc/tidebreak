# 16. Provider failures preserve account-access detail

- Status: Proposed
- Date: 2026-08-14
- Owners: provider routing and desktop chat
- Related: `crates/tidebreak-router/src/sse.rs`, `crates/tidebreak-server/src/event_projection.rs`
- Supersedes: none

## Context

Tidebreak currently classifies every provider HTTP 401 and 403 as an
authentication failure. The desktop then turns that category into a definitive
claim that the API key is invalid. Providers also use 403 for exhausted credits,
organization billing restrictions, missing model access, and key permissions.
The current message therefore sends users to rotate a valid key and obscures
whether Tidebreak, the provider, or the user's provider account rejected the
request.

Turn failures already retain a bounded error detail. The renderer projection
discards it even when it was produced by the provider-error sanitizer, leaving
the UI with only a coarse category.

## Decision

Treat a bare provider 403 as a distinct, terminal `provider_access` failure.
Reserve `auth` for a 401, an explicit provider authentication code, or a locally
missing credential. Provider access failures do not retry automatically because
the same account state will ordinarily reject the same request again.

The renderer contract carries an optional bounded detail for provider-originated
failures. The desktop shows that detail directly and explains which system made
the decision. It may name likely account causes such as credits, billing,
organization policy, model access, or key permissions, but must not assert one
without a provider code or message that establishes it. Existing credential
redaction remains as defense against a provider echoing secret material.

Internal storage, filesystem, and invariant details remain excluded from the
renderer contract.

## Alternatives Considered

- Keep the wire unchanged and soften the `auth` sentence. Rejected because it
  still cannot distinguish an invalid key from provider-account access and
  continues to hide useful provider diagnostics.
- Treat every 403 as quota exhaustion. Rejected because 403 is also used for
  organization policy, model entitlements, billing, and permissions.
- Expose every terminal error verbatim. Rejected because non-provider failures
  can include host paths and internal diagnostics that are irrelevant to the
  user-facing recovery path.

## Consequences

The event and transcript wire types gain an optional failure detail and a new
failure category. Clients must handle `provider_access` explicitly. Users get a
stable explanation after both live failure and transcript hydration, and can
see the provider's bounded response without opening a debug bundle.

Revisit this decision if providers converge on a reliable structured taxonomy
for credits, billing, entitlements, and organization policy; those states could
then become separate actionable categories.

## Validation

- A bare xAI 403 projects as `provider_access`, not `auth`, and does not offer a
  retry.
- A 401 and explicit `invalid_api_key` still project as `auth`.
- A bounded provider message survives live projection and transcript hydration.
- A store or invariant failure does not expose its detail to the renderer.
- The desktop identifies the provider as the rejecting system and presents
  credits/quota as a possibility rather than a fact when only a bare 403 exists.
