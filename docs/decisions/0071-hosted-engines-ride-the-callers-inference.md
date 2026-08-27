# 71. Hosted engines ride the caller's inference

- Status: Proposed
- Date: 2026-08-26
- Owners: server
- Related: [`0051-on-behalf-of-inference-for-hosted-machines.md`](0051-on-behalf-of-inference-for-hosted-machines.md),
  [`0062-per-caller-gateway-entitlements.md`](0062-per-caller-gateway-entitlements.md),
  [`0063-hosted-machines-borrow-forge-credentials.md`](0063-hosted-machines-borrow-forge-credentials.md),
  [`0064-idle-engine-children-are-parked.md`](0064-idle-engine-children-are-parked.md)

## Context

A code session runs a real engine child — Claude Code, Codex — and that
child makes its own inference requests with its own HTTP client. On a
desktop or self-host machine the child finds the operator's provider
credentials the way it always has: a login the operator ran, or a key in
the environment.

A gateway-authenticated hosted machine has neither. The image carries no
provider keys, nobody has run a harness login inside the container, and
decision 51's on-behalf-of exchange lives in the server process, never in
the child. The server spawned engine children with empty `extra_argv` and
`extra_env`, so a hosted Codex session fell back to `api.openai.com`, first
over its websocket transport (logging a reconnect notice per attempt) and
then over HTTPS, and failed the first turn with a 401.

Handing the child a credential directly does not fit the tokens we have.
The caller's machine-bound token must never leave the server process, and
an exchanged inference token expires in minutes while a session lives for
days. Whatever the child holds has to be durable for the session and
worthless anywhere else.

## Decision

1. **The server relays engine inference through the caller's grant.** Two
   routes on the main listener, `/code/llm/anthropic/v1/messages` and
   `/code/llm/openai/v1/responses`, stream requests through to the Model
   Gateway's compat endpoints. Per request, the relay exchanges the owning
   caller's live machine-bound token for a fresh inference token (the same
   single-flight cache decision 51 uses for chat) and sends that upstream
   in place of what the child presented. The session outlives any single
   token because the exchange runs on every request.

2. **The child authenticates with a session-scoped relay key.** At spawn on
   a machine that has an on-behalf-of gateway, the runtime mints an opaque
   `tbreak_hl_` key mapped to (owner, session) and wires the child to the
   relay: Claude Code through `ANTHROPIC_BASE_URL` and
   `ANTHROPIC_AUTH_TOKEN`, Codex through a `tidebreak` model provider on
   the command line (`base_url`, `env_key`, `wire_api=responses`). The key
   follows the browser channel's lifecycle exactly — reissue on attach
   replaces the prior key, and every path that revokes the browser channel
   revokes the relay key. The routes live outside `require_token`, and the
   key opens nothing but the relay.

3. **Everywhere else, nothing changes.** A machine without an on-behalf-of
   gateway spawns children with empty `extra_argv` and `extra_env`, exactly
   as before. The custom Codex provider also keeps hosted sessions off the
   vendor-only websocket transport, which removes the reconnect noise.

Refusals are vendor-shaped so the engine reports them legibly: 401
`authentication_error` for a missing or revoked key and for a caller whose
gateway session is gone (fail fast, no retry); 502 `api_error` when the
gateway does not answer (the engine's own retry policy applies).

## Alternatives considered

- **Inject an exchanged inference token at spawn.** Dies when the token
  expires, minutes into a session that lives for days, and a reap or
  relaunch would hand the new child a stale credential.
- **Run harness logins in the hosted image.** Provider accounts per
  machine, not per caller: every member's sessions would spend one shared
  account, against the whole thrust of decisions 51/62/63/65.
- **Point the child at the gateway's compat endpoint directly.** The child
  would need a durable gateway credential, which is exactly the thing the
  machine-bound token model refuses to mint; the relay keeps the only
  durable secret local and session-scoped.

## Consequences

- Hosted Claude Code, Codex, OpenCode, and Grok sessions run turns as the
  caller, with the caller's entitlements and metering, matching chat.
- A Codex thread started before this decision recorded the default
  provider in its rollout; resuming it may still fail. A new session is
  the path.
- The relay streams request and response bodies without buffering, with a
  raised body limit on its routes, so long turns and large contexts pass
  through unchanged.

## Validation

- `spawn_wiring_points_each_engine_at_the_relay` pins the per-engine argv
  and environment.
- `forward_exchanges_and_streams_for_a_live_caller` drives a request
  through a fake gateway and asserts the exchanged token replaces the
  relay key, the query string and body pass through, and the child's
  credentials never reach upstream.
- `issue_replaces_the_prior_key_and_revoke_forgets`,
  `forward_refuses_an_unknown_key`,
  `forward_fails_closed_without_a_caller_token`, and
  `forward_reports_an_unreachable_gateway_as_502` pin the lifecycle and
  the refusal shapes.
- Both wirings were exercised against real engine binaries before this
  was written: Codex 0.147.0 with the custom provider makes a single
  `POST .../responses` with the key as bearer and never touches its
  websocket transport; Claude Code with the base-URL override posts
  `/v1/messages?beta=true` with the bearer and no `x-api-key`.
