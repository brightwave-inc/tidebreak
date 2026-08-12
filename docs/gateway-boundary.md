# The OpenWave ↔ model gateway boundary

How the desktop app becomes a client of a model gateway deployment, and what
crosses the wire once it is one. This page is the design overview; the
enforcement details live in the module documentation of
`crates/openwave-server/src/managed_policy.rs`,
`crates/openwave-server/src/pairing.rs`,
`crates/openwave-desktop/src/deep_link.rs`, and
`crates/openwave-server/src/connectors/gateway.rs`. The per-platform MDM artifacts
are in [managed-policy.md](managed-policy.md); the MCP transport details are
in [mcp-servers.md](mcp-servers.md).

## Posture

OpenWave ships as one artifact. Whether a given profile is gateway-managed is
runtime state, not a build flavor, and everything the gateway controls —
which models exist, which MCP endpoints mount, whether provider keys are
editable — is enforced twice, with different weight on each side:

- **Server-side is the real control.** Entitlements, token audiences, and
  endpoint attestation are decided by the gateway per request. A tampered
  client gets refusals, not access.
- **Client-side lockdown is a product boundary.** Hiding the Providers panel
  and refusing manual MCP servers on a managed profile keeps the product
  coherent; it is not a security mechanism and is documented as such.

Two layers of local state implement the client side, and they are
deliberately separate:

- **Policy** — *which gateway manages this profile.* Durable, survives
  sign-out.
- **Session** — *an authenticated user at that gateway.* Revocable,
  replaceable, held in the secret store.

Disconnecting drops the session and never the policy, which is why a
signed-out managed profile lands on the sign-in gate rather than the open
product.

## How a profile becomes managed

Policy resolution has three tiers, strongest first:

1. **OS-asserted (MDM).** The per-platform managed artifact names a gateway
   base URL. This tier is read fresh on every resolution, is never written
   by the app, and is never replaceable from inside it. A present-but-broken
   artifact fails closed: managed-but-misconfigured, with no usable gateway
   and no fallback to the open product.
2. **Provisioned.** The sticky policy file `gateway-policy.json` in the
   profile's data directory, written only by a completed pairing (below).
   User-consented, and replaceable by the same consent that created it. It
   is deliberately a sidecar file rather than a database row: a pre-v1
   schema-epoch reset deletes the SQLite profile, and the policy must
   survive that — losing it would resolve the profile unmanaged and orphan
   the session below. Profiles paired before the move are imported once at
   boot from the legacy `managed_policy_v1` settings row, which the next
   epoch reset then removes naturally.
3. **Open.** Neither present; the unmanaged product, which has no gateway
   surface at all.

## Pairing: the provision link is a proposal, not a command

Pairing starts from the gateway's own web UI: a link of exactly one shape,
`openwave://provision?gateway=<base url>` (`openwave-dev://` in debug
builds). A deep link is an unauthenticated remote trigger — any page can
raise one, and a custom scheme carries no provenance — so the boundary rule
is that **the link never writes anything**:

- The shell validates the link strictly and *registers* it as a pending
  pairing: an in-memory slot on the server runtime. Nothing durable exists,
  and the named gateway is not even probed. The base URL must be `https`
  unless its host is loopback (`localhost`, `127.0.0.1`, `::1`), which is
  the developer-deployment exception — the pairing spends an OAuth code and
  stores the tokens it buys, so cleartext to anywhere but this machine would
  hand the whole exchange to whoever is on the path.
- The sign-in gate presents the pending pairing full-window. The consent is
  the sign-in: only an OAuth flow the user completes against that gateway
  commits the provision, inside the exchange's finish, before the session is
  stored. Dismissing ("Not now") clears the slot and returns the app.
- A link naming a *different* gateway than the provisioned one escalates to
  a native confirmation naming both origins. Confirming parks a *replacing*
  pairing that remembers what it expects to replace; the commit is a
  compare-and-swap, so a policy that moved mid-flow refuses rather than
  clobbers. An OS-asserted gateway is never replaceable this way.
- Registration and commit are called on the embedded server's handles
  directly from the shell. No HTTP route reaches the policy write path, and
  the webview cannot forge the shell's events — the window capability denies
  event emission renderer-side. The shell does emit a best-effort
  `gateway:pairing-changed` nudge the other direction so the gate refetches
  promptly; the gate's policy poll remains the fallback.

The result is that a drive-by link can, at worst, raise a sign-in screen the
user ignores, or one dialog that defaults to changing nothing.

## Authentication

OpenWave is a registered first-class OAuth client of the gateway, client id
`openwave`. Sign-in is an authorization-code + PKCE flow in the user's
default browser against the gateway's `/oauth/authorize`, redirecting to a
single-use loopback listener (`http://127.0.0.1:<random port>/callback`);
the app never sees credentials, only the code. The requested scope is
`openid profile offline_access models:read inference:invoke`.

Tokens are minted per **audience**: `control` for profile and entitlement
reads (`/api/v1/cli/*`), `llm` for inference on the gateway's
protocol-compatible routes, and per-endpoint `mcp:<slug>` bearers for
gateway MCP connections. Access tokens are cached and refreshed lazily
through a rotating refresh grant; the client name rides the token request so
gateway-side usage attribution names the client rather than `generic`.

The durable credential — refresh token, installation id, account hint,
cached access tokens — lives in the profile's secret store under
`gateway.credentials_v1`. Three invariants govern it:

- **A session is pinned to the policy's gateway.** A stored session whose
  base URL no longer matches policy is refused (`sign-in required`) and
  retired at boot: best-effort revoke at its own gateway, unconditional
  local clear.
- **A superseding pairing retires the old session before storing the new
  one.** A committed re-pair revokes at the old gateway (bounded, best
  effort) and clears locally; only then is the new session stored.
- **`invalid_grant` means sign in again, not retry.** Refresh-token reuse
  detection or revocation surfaces as a reconnect affordance. Known edge:
  the credential is not auto-cleared on that failure, so the status surface
  keeps reporting a signed-in session whose calls fail until the user
  reconnects or disconnects.

## What crosses the wire

The connector speaks a small, versioned HTTP surface on the gateway:

- `/api/v1/meta` — unauthenticated deployment identity, shown before and
  during sign-in.
- `/api/v1/cli/me` — the authenticated user, for the account hint.
- `/api/v1/cli/models` — the models this user may invoke. OpenWave syncs the
  full list and retains each row's `protocol` (`anthropic_messages` or
  `openai_responses`) in a local snapshot stamped with the gateway URL
  after sign-in and on the explicit "Refresh models" affordance; gateways
  predating the field are treated as Anthropic-only. The snapshot is what the
  model picker offers, and sign-out empties it. A stamp that no longer matches
  policy invalidates the snapshot rather than serving another gateway's
  models.
- `/api/v1/cli/apps` — entitled connected apps
  (`id`, `name`, `app_kind`, `enabled`, `mcp_endpoint_slugs`), listed in
  the gateway settings panel. A `404` means an older gateway; the section
  hides instead of erroring.
- `/api/v1/cli/shared-apps` — registering a gateway-bound local app as a
  draft, appending each later revision, and relaying a viewer's consent and
  the app's own operation calls. The gateway's **publish** route is
  deliberately absent: publishing is a governance action done on the
  gateway's web surface (decision record 11), and no code path here calls it.
  What this host resolves instead is the address of the app's page there, so
  the author can be handed over to it.
- `/oauth/authorize`, `/oauth/token`, `/oauth/revoke` — the flow above.
  A refresh grant may also declare an `attestation_context_id`: a
  client-minted random UUID naming the chat's attestation context, one per
  chat plus one shared connect context for MCP handshakes. It correlates a
  chat's inference tokens with its MCP tokens so gateway-attested endpoints
  can match tool calls against model-emitted observations; it carries no
  chat content and is meaningless outside the session it is pinned to.
- Inference itself — invoked with `llm`-audience bearers through the matching
  gateway surface: Anthropic Messages at `/compat/anthropic/v1/messages`, or
  OpenAI Responses at `/compat/openai/v1/responses` — the only two surfaces
  a gateway serves northbound; it has no Chat Completions route. Both use
  the same provider machinery as their direct counterparts and fetch the
  rotating bearer per request. A turn also declares which conversation it
  belongs to, as an `x-model-gateway-conversation-id` header carrying the
  chat's id, so the gateway's usage views group inference the way the app
  does. The header is a per-route opt-in: only gateway adapters send it, a
  direct provider never does, and it is a header rather than wire data, so no
  model receives it. The chat id is the only thing declared — no title, no
  content, no participant.
- MCP — `{base}/mcp/{slug}` per mounted endpoint. The connection minting
  its `mcp:<slug>` bearer at connect time rides the shared connect
  attestation context, and each `tools/call` presents a bearer minted
  inside the calling chat's context instead. Gateway-attested endpoints
  accept the handshake on any context-bearing token but a tool call only
  when it consumes the matching model-emitted observation; that check is
  the gateway's, not the client's. Chats on non-gateway models and
  local-app invokes carry no matching observation, so attested endpoints
  refuse those calls by design.

Everything here degrades independently: a gateway that is down fails model
sync, inference, and MCP connects with visible errors, while the local
session state remains a local read — the status surface reports the stored
session, not liveness. "Refresh models" doubles as the cheapest reachability
probe the UI offers.

## Who may touch what

- **The shell** (Rust, Tauri) owns link validation, the native
  confirmation and refusal dialogs, `pairing.log`, and the only path into
  pairing registration.
- **The embedded server** owns policy resolution, the pending-pairing slot,
  the OAuth exchange, the commit, session storage, and lockdown
  enforcement. Renderer-reachable routes (`/policy`, `/gateway/*`) expose
  status, sign-in/sign-out, dismissal, and model sync — none of them can
  write policy.
- **The renderer** presents: the sign-in gate holds the window while policy
  demands it and lifts only for a session on the policy's own gateway. It
  re-reads `/policy` on a poll and on the shell's nudge, and treats a
  missing policy route as the open product — a renderer running ahead of an
  older server has nothing to enforce.

The division is the boundary's summary: the gateway decides entitlements,
the server decides state transitions, the shell mediates consent, and the
renderer only ever renders the result.
