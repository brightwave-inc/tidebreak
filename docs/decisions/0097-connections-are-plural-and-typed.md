# 97. Connections are plural and typed

- Status: Proposed
- Date: 2026-09-14
- Owners: mobile
- Related: [`0072-mobile-client.md`](0072-mobile-client.md) (amended by this
  record), [`0049-gateway-authenticated-hosted-machines.md`](0049-gateway-authenticated-hosted-machines.md),
  mg ADR 0060, GitHub epic #3398, brightwave-inc/model-gateway#2044
- Supersedes: none

## Context

Decision 72 gave the mobile client one gateway session and one machine, held in
a single secure-store blob (`tidebreak.mobile.session.v1`) owned by one
`TokenStore`. That was the right shape for pairing plus attach.

Epic #3398 folds Model Gateway's Tidewatch app into this one. Tidewatch holds
several gateway pairings at once, each with its own credential, and switching
between them switches the whole session — identity, authority, and what the
navigation offers. The merged app also grows a second way to reach a machine:
standalone pairing (#3404), a direct URL with a static token and no gateway in
the path at all.

Two facts about the gateway bound what the app may do today.

Its `tidebreak-mobile` OAuth client is confined to the `control` and
`tidebreak:*` resources, and is registered as not control-plane capable
(`REGISTERED_CLIENTS`, `crates/server/src/cli_auth.rs`).
brightwave-inc/model-gateway#2044 widens it; until that is deployed, the
authorize page refuses a `control_plane:*` scope from this client outright —
not the console surface, the whole sign-in — and the token endpoint refuses a
console resource with HTTP 400 `invalid_resource`.

The advertisement that exists on `/api/v1/meta` today,
`surfaces.control_plane_write`, is a binary-wide fact about the authorization
server's scope vocabulary. It is already true on gateways whose
`tidebreak-mobile` client is still confined, so it cannot stand in for "this
client may hold console resources".

## Decision

1. **A connection is the unit, and there are several.** `Connection` is a
   discriminated union on `kind`. `gateway` — an OAuth pairing carrying the
   gateway URL, installation id, attached machine, identity, and granted scope
   — is the only kind implemented. `machine` is declared and deliberately
   unbuilt, so #3404 adds a member rather than re-cutting the store. Several
   connections coexist; exactly one is active.
2. **Credentials are per connection.** Each connection owns a secure-store key
   (`tidebreak.mobile.connection.<id>`) holding its durable facts and its
   rotating refresh token, and one `TokenStore` instance with its own refresh
   queue and per-resource single flight. A directory key
   (`tidebreak.mobile.connections.v2`) records the ids and which is active.
   Access tokens stay memory-only. Signing out of one connection deletes its
   key and forgets it; every other connection stays signed in.
3. **Connection ids are derived, not random:** the gateway's installation id,
   or a hash of its canonical URL when it names none. Pairing the same
   deployment twice replaces that connection instead of stacking a second live
   refresh family for one gateway.
4. **The v1 blob migrates, once.** The first hydrate with no directory adopts
   `tidebreak.mobile.session.v1` — gateway, machine, identity, and the rotating
   refresh token — as a gateway connection and deletes it. An install upgrading
   into this model keeps its pairing.
5. **A reinstall wipes credentials.** iOS Keychain entries outlive the app that
   wrote them. The directory records the OS-reported install time; a different
   one on launch wipes every credential before anything is read. An unknown
   install time never wipes.
6. **The resource allowlist is the union of the two apps**: `control`,
   `control_plane`, `tidebreak:<digest>`, `runtime:<slug>`. A gateway that
   refuses one answers `invalid_resource`, which the client raises as a
   distinct `ResourceRefusedError` — the surface that asked degrades, the
   pairing stands. Only `invalid_grant`-shaped refusals sign a connection out.
7. **Scope is requested only where it is advertised.** The baseline stays
   `openid profile offline_access`. `control_plane:read` and `runtime:execute`
   are requested only from a gateway advertising
   `surfaces.tidebreak_mobile_console`, and `control_plane:write` additionally
   requires `surfaces.control_plane_write`. No gateway advertises the first
   flag yet, so this app asks for exactly what it asks for today until the
   widened client and its advertisement deploy.
8. **The session's granted scope is recorded at sign-in and carried through
   every rotation.** A rotation's response describes the resource it minted,
   never the session, so it never restates authority. An unrecorded grant reads
   as no authority.
9. **Navigation gates on the active connection's kind and grant**
   (`src/lib/sections.ts`), not on a per-screen condition: machine surfaces
   need an attached machine, console surfaces need a gateway connection whose
   recorded grant carries the console scope.

## Alternatives Considered

- **Keep one session and add a second one later.** Rejected: the single-session
  assumption is load-bearing in the token store, the routing gate, and every
  screen. Paying for plurality once, before the port slices land on top of it,
  is cheaper than paying for it in each of them.
- **One blob holding every connection.** Rejected: `expo-secure-store` caps a
  value near 2KB on Android, and a shared blob makes signing out of one
  connection a read-modify-write of another's credential.
- **Mirror Tidewatch exactly — pairings in AsyncStorage, credentials in
  SecureStore.** Rejected here: it adds a dependency for a split this app does
  not need, since a connection without a credential has nothing to show. The
  useful half of that design, a credential key per installation, is kept.
- **Use `AsyncStorage`'s absence as the first-launch marker, as Tidewatch
  does.** Rejected with the dependency. The OS-reported install time changes at
  exactly the same moment and needs nothing new.
- **Gate console scopes on `surfaces.control_plane_write`.** Rejected: it is
  true on gateways that still confine this client, so it would break sign-in
  everywhere rather than degrade the console.
- **Request the console scopes unconditionally and handle the refusal.**
  Rejected: the refusal is at the authorize page, which fails sign-in. There is
  nothing left to degrade into.
- **Random connection ids.** Rejected: re-pairing a gateway would leave the
  previous refresh family live and unreachable on the device.

## Consequences

- Decision 72 stands except for its single-session framing: point 3's
  "one serialized refresh queue per gateway session" now reads per connection,
  and point 2's resource list is the widened allowlist above.
- Console surfaces (#3400–#3402) can be built against the connection model
  before the gateway widens, and they will render for nobody until a gateway
  advertises `surfaces.tidebreak_mobile_console` and a user signs in again.
  Widening authority always costs a re-authentication; nothing upgrades in
  place.
- That advertisement does not exist yet. The gateway half of the epic
  (brightwave-inc/model-gateway#2044) must add it alongside the widened client,
  or the app's console scopes stay unrequested no matter how the client is
  registered.
- An install that upgrades into this model is migrated rather than wiped,
  because it has no recorded install time to compare against. It is therefore
  the one reinstall this decision cannot catch; every launch after it is
  stamped.
- Revisit if the gateway starts advertising client confinement directly (the
  flag becomes redundant), if connections stop being few enough to hold one
  secure-store key each, or if a kind arrives whose credential is not a rotating
  refresh token in a way `TokenStore` cannot express.

## Validation

- Two gateway pairings coexist; switching changes the active machine and the
  credential family the next mint rotates.
- Signing out of one connection deletes only its credential, and the other
  connection still mints.
- A revoked refresh family drops exactly the connection that held it.
- A v1 blob is adopted with its refresh token and disappears from storage; a
  second hydrate does not resurrect it, and the summary the UI reads carries no
  credential.
- A changed install time wipes every stored credential; an unknown one wipes
  nothing.
- `requestedScope` returns the baseline for a gateway advertising nothing, and
  for one advertising only `control_plane_write` — the case a plausible wrong
  implementation would pass by treating that flag as console permission.
- A 400 `invalid_resource` on a console mint leaves the session intact and the
  machine resource still mintable; a 401 signs out.
- The granted scope recorded at sign-in survives a rotation whose response
  names a narrower scope.
