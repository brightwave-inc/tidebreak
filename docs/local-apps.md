# Local apps

Agent-generated mini-apps that live in the profile: a bounded HTML document
rendered in the sandboxed view-frame machinery, plus a manifest that pins the
exact connected-app operations and folders the app may call. The user asks
for "a Sentry triage view" once; the result is a durable, revisable surface
they can reopen from the sidebar instead of a conversation they re-run.

This page is the design overview for the local-first slice. Sharing is
deliberately out of scope: the eventual sharing plane is a model gateway
("promotion", sketched at the end), not any peer-to-peer exchange.

## Product flow

1. In a conversation, the foreground agent calls `create_app` with a name, an
   HTML bundle, and a manifest naming the capabilities the app uses. The
   transcript shows an app card; revisions of the same app append, never
   overwrite.
2. The user opens the app — from the card or from an **Apps** library on the
   home sidebar. First open (and any open after the app's capability needs
   change) presents a consent sheet listing exactly what the app may call.
   Consent is per-app, durable, and revocable from the library.
3. The app renders in a sandboxed frame and drives its pinned capabilities
   through the host. Results render however the app likes; the app never
   holds a credential and has no network of its own.

## Two parts, two trust levels

An app revision is an untrusted **bundle** and a trusted **manifest**:

- The bundle is model-authored HTML/JS/CSS, at most 1 MiB (the same bound as a
  prefetched MCP view). It is assumed hostile: prompt injection anywhere in a
  conversation can author it.
- The manifest is small, structural JSON: a display name and
  `bindings: [{ app, operation_ids[] }]` naming declared operations under the
  connected-app records ([connected-apps.md](connected-apps.md)) that
  contribute them, `{ folder, access }` naming a connected folder, or
  `{ gateway_app, operation_ids[] }` naming a connected app the model gateway
  holds (record 10). The manifest — not the bundle — is what the user consents
  to and what the host enforces per call.

  The gateway shape resolves against the signed-in gateway session: the
  `create_app` roster lists each entitled app with the operation ids the
  gateway declares for it, the door refuses an unknown id or an undeclared
  operation, and consent pins the gateway origin, the app id, and a hash of
  that catalog. Every input is read live per request — nothing about a gateway
  app is cached locally — so a profile with no session, an app that loses its
  entitlement, or a re-ingested catalog all fail closed to re-consent. Invoke
  relays the call to the gateway's shared-app route on a `control`-audience
  session bearer — after the local ladder passes, never before — and the
  gateway re-enforces entitlement, its own manifest pin, and credential
  resolution live per call. A viewer who still needs to connect the bound
  app at the gateway gets the typed `gateway_authorization_required`
  refusal (the detail page offers the connect affordance); a profile whose
  gateway session, deployment, or draft registration cannot answer gets
  `gateway_unavailable`, with the message naming which.

  A relay needs a shared app at the gateway to relay *to*, and the host
  establishes that itself rather than asking the author to. Consenting to a
  manifest that binds a gateway app registers the app at the deployment
  policy names and relays the author's consent for it; both are best effort
  after the grant is already durable, because a gateway that is down must
  never cost the user their consent. The first invoke does the same work if
  that could not, so a granted app heals into a servable one on its own. The
  mapping is stored per `(app, deployment)`, so a profile re-paired to a
  different gateway holds no registration there and registers afresh —
  exactly as it holds no gateway grant there. Revision sync is lazy: the local
  revision about to be served is pushed on the next relay, and revisions
  nobody ever invoked are never pushed, so the gateway's history is what was
  servable when it was used. The gateway's own `consent_required` refusal is
  healed once — the consent sheet already displayed exactly the binding set
  the gateway consent names, and the gateway recomputes that set server-side
  from the live revision — and a second refusal is reported as the gateway's
  answer rather than retried.

The containment story is inherited from MCP App views and unchanged: the frame
is served by the host with its own strict CSP (`default-src 'none'`,
`connect-src 'none'`), sandboxed `allow-scripts` only, opaque origin. The
bundle cannot fetch, cannot reach Tidebreak's DOM, storage, bearer token, or
IPC, and cannot talk to anything except its parent via `postMessage`. A
malicious bundle can render lies; it cannot exfiltrate, and it cannot call
anything the manifest did not pin and the user did not grant.

Because the frame's CSP forbids all network, the app **never calls the host
API directly**. Every tool call is a `postMessage` to the parent; the trusted
renderer forwards it to the bearer-authenticated invoke route and posts the
result back. The renderer treats both directions as opaque passthrough JSON —
the same posture as the existing MCP App payload, and for the same reason:
canonical tool arguments and results are model- and remote-authored content
the renderer must never interpret.

## The app record

Apps follow the durable-output discipline but are **profile-scoped**: an app
outlives the conversation that created it. Outputs cannot express this — their
rows and bytes are keyed to a chat — so apps get their own record:

- An `app` row: opaque id, display name, current revision, revision count,
  soft-delete. No chat foreign key; the profile owns it.
- Insert-only `app_revision` rows: opaque id, one-based ordinal, manifest
  JSON (bounded), bundle byte length and SHA-256, and producer attribution
  (the turn or run that authored it, plus the originating conversation as
  provenance). Revisions are capped; reaching the cap refuses the write.
- Bundle bytes at `apps/{app id}/{revision id}` under the profile data
  directory — write-once, path derived only from durable identity, symlink-
  refusing, never under any conversation's private scratch.

`create_app` follows the outputs record's identity discipline: ids derived
from the durable tool-call id, so an ambiguous store response retries into the
same record instead of a duplicate. Authoring always happens inside a turn, so
call identity exists; it is only *invocation* that happens outside one.

## Invocation and consent

Invocation is new host surface: the first tool execution outside a model turn.
The route (`POST /apps/{id}/invoke`, renderer bearer) executes the named
capability through the host's governed dispatch — mechanically the same
execution a turn performs, minus the turn.

What it deliberately does **not** reuse is the chat approval gate. That gate
is enforced inside the foreground agent loop, scoped to a chat and its
permission mode, and MCP calls are approvable per-call precisely because a
Settings edit can swap the process behind a stable namespace — a name-based
standing grant would silently widen. None of that maps onto a profile-scoped
app driven by a human click. Local apps therefore get their own consent
object, designed against the same threat:

- An **app grant** records, per app: the granted `(app, capabilities[])` set —
  keyed by connected-app record id — and a **fingerprint of each bound app's
  definition** (whatever the kind carries: base URL, document hash, and
  credential reference for `rest_api`; namespace, command, args, cwd,
  selected env names, URL for the retired `mcp_server` vocabulary).
- The grant is created by explicit user consent on a sheet that lists every
  binding. It is revocable from the Apps library. A gateway row is presented
  as the network access it is — it counts toward the combined-consent
  exfiltration warning exactly as a local operations row does — and carries a
  qualifier saying the call runs through the organization's gateway as the
  signed-in user. The row names the gateway app's id, its display name, and
  the pinned operation ids; the gateway's own URL never reaches the renderer,
  the same names-only posture the `rest_api` rows hold.
- Every invoke checks, live: the tool is in the current revision's manifest,
  the manifest entry is covered by the grant, **and** the bound server's
  current definition still matches the granted fingerprint. A reconfigured
  server invalidates the grant and the next open re-prompts, so consent can
  never outlive the thing it named — the exact property that makes per-call
  approval mandatory in chats, preserved without per-call friction.
- A revision that changes the manifest exceeds the grant by construction and
  re-prompts.

Chat permission modes and plan mode do not apply: there is no chat. The grant
is the whole policy, and it fails closed — no grant, no call, and an
ungranted or unpinned tool name is refused server-side regardless of what the
renderer asked for.

This does widen what the renderer bearer can do: today the renderer can only
approve model-initiated calls; with apps it can initiate granted ones. The
widening is bounded — the server enforces pin + grant + fingerprint, so a
compromised renderer gains only the tool set some installed app pinned and
the user granted — and the bearer already reaches settings, credentials, and
policy surfaces, so this is not a new class of authority (see the
authorization gate below).

Results are clamped by the existing MCP result bounds before they cross to
the renderer, and cross it as opaque passthrough.

## Frame serving

The view-frame machinery serves app revisions through the same mint/redeem
shape: the renderer trades its bearer for a single-use, minute-lived frame
token; the unauthenticated frame route redeems it once and serves the
document with the strict CSP. Two changes from today's implementation:

- The token payload grows a source: a prefetched MCP view addressed by
  `(server, uri)`, or an app revision addressed by durable identity.
- The token table moves off the MCP runtime into its own small type in app
  state; the MCP runtime has no other reason to be involved in app frames.

The frame's opaque origin means an app has no storage of its own. v1 apps are
stateless: they refetch through their tools on open. A state primitive is a
deliberate non-goal until something real needs it.

## Tool bindings are retired

Mounted MCP tools were the first binding vocabulary — the only one available
when local apps shipped — and are no longer bindable (#1332). The
connected-apps epic ([connected-apps.md](connected-apps.md), #1330) made MCP
servers one *kind* of connected app and added the `rest_api` kind — a base
URL, an ingested OpenAPI catalog, and a secret-store credential, executed
through a governed local egress layer. App manifests bind
`{ app, operation_ids[] }`, with the grant, consent-sheet, and invoke
machinery on this page carried over unchanged. That is now the one callable
surface.

With REST in place, tool bindings had no remaining use their vocabulary
covered better, and their consentable universe was the wrong shape for apps:
a manifest could pin tools of *any* mounted transport — local stdio, remote
HTTP, or gateway endpoints — putting "run a local process on this machine"
inside the app-grantable world, while gateway-attested endpoints refused
app-driven session-token calls regardless. #1332 first refused the vocabulary
at every door (authoring, consent, invoke) while keeping stored manifests
parseable; #1589 then removed it end to end — the types, the shared binding
grammar's tools arm, the invoke route's tool surface, the frame bridge's
`tools/call` verb, and the wire shapes are gone, and the pre-v1 schema epoch
was bumped so profiles carrying the old vocabulary are rebuilt.

Mounted MCP servers themselves are unchanged: chats keep their full tool
surface. The retirement narrows what an *app manifest* may pin, nothing
else.

## The authorization gate

The invoke route rides the per-launch loopback bearer, which is a capability
check on the local process, not a principal (#853). That is the correct
posture for the single-user desktop and the wrong one for anything shared.
Local apps inherit it knowingly: nothing here adds per-user authorization,
and #853 remains the recorded gate before any deployment that puts more than
one person behind one server — apps included.

## Sharing: registered here, published at the gateway

A gateway-bound local app is auto-registered at the model gateway as a
**draft** — a shared app the author alone reaches — on the first relay or
consent, and each later local revision is appended lazily, so the gateway's
history is what was servable when it was used. From there the gateway stores
everything sharing-related: the draft and its revisions, the published status,
and the per-team grants that make it openable.

**Publishing is not done here.** Decision record 14 places it on the gateway's
own web surface, next to the publish state, the grants, and the revocation it
belongs with. Publishing mutates gateway-owned entitlement state; every other
mutation of that state already happens at the gateway, and a publish dialog in
each harness that authors apps is a second interface of record that can only
drift from the first. So the app page's **Publish at gateway** affordance
resolves the app's page at the deployment holding it —
`POST /apps/{id}/gateway-page`, which registers the draft if it never has been
— and opens that page in the browser. Nothing in Tidebreak calls the gateway's
publish route.

Three properties make the registration a copy rather than a translation:

- **The binding vocabulary maps.** A gateway binding already names what the
  gateway's own shared-app manifests name — record 10 rejected the
  translate-at-publish design, because a manifest rewritten at share time was
  never run the way its viewers will run it. Folder and local `rest_api`
  bindings are dropped rather than translated: they name capabilities that
  exist only on this machine. An app bound to nothing at the gateway cannot be
  shared at all, and is refused with `app_not_gateway_bound` rather than
  registered as an empty shell.
- **Revision identity carries.** Digests, ordinals, and producer provenance
  become the published revision's provenance.
- **Attestation flips from limitation to benefit.** Once the gateway brokers
  the calls, attested apps become reachable — a concrete reason to share
  beyond sharing itself.

## Non-goals

- **No server component.** A local app is a client over governed tool calls;
  anything that needs computation is the agent's job, in a conversation.
- **No background execution.** An app runs while the user is looking at it.
  Autonomous behavior has no viewer to attribute and is a gateway-side
  service-principal question, deferred there too.
- **No peer-to-peer sharing.** The gateway is the sharing plane.
- **No app state primitive** until a real app needs one — though a
  folder-writing app ([folder-bindings.md](folder-bindings.md)) can keep
  its state in a granted folder, which is the interim answer.

## Slices

1. App record: `app` + `app_revision` tables, store ops, write-once bundle
   bytes under the profile `apps/` root.
2. Frame serving for app revisions: token source enum, token table lifted off
   the MCP runtime, same CSP and single-use contract.
3. Invoke route: bearer-guarded, registry-snapshot dispatch, manifest pin
   enforcement, clamped opaque results.
4. App grants: the consent object, server-definition fingerprints, live
   invalidation, consent sheet, revocation.
5. `create_app` tool and transcript card.
6. Apps library: home-sidebar entry, panel type, list/detail views, the open
   flow wiring frame + bridge + invoke together.
