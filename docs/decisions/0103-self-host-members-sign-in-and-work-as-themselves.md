# 103. Self-host members sign in and work as themselves

- Status: Proposed
- Date: 2026-09-24
- Owners: server, security
- Related: [0006](0006-self-host-deployment-plane-authorization.md), [0034](0034-harness-discovery-credentials.md), [0049](0049-gateway-authenticated-hosted-machines.md), [0056](0056-one-credential-item-per-profile.md), [0063](0063-hosted-machines-borrow-forge-credentials.md), [0065](0065-hosted-git-acts-as-the-person.md), [0071](0071-hosted-engines-ride-the-callers-inference.md), [0082](0082-the-hosted-machine-serves-the-renderer.md), [0086](0086-session-access-is-separate-from-ownership.md), [0087](0087-standalone-browser-sign-in.md), [0089](0089-service-principals.md), [0090](0090-a-session-acts-as-one-forge-identity.md), [0102](0102-self-host-secrets-in-the-database.md), #3590
- Supersedes: decision 6's exclusion of per-user provider credentials and its
  rejection of members bringing their own keys. Its two planes stand.

## Context

A standalone self-host machine names the principal behind every request, from
the token file or an OpenID Connect provider, and scopes data to that owner
(decisions 6 and 87). The work a principal starts does not run as them:

- Code mode runs every member's terminals, setup scripts, and engines as the
  server's account, uid 10001 (#3590). Terminals inherit the server's
  environment, database URL included, and every such process can read `tokens`,
  the key file, other members' checkouts, and other sessions' relay keys.
- Every engine session spends the one sign-in an administrator made in the
  shared home, or the provider key in the server's environment.
- Decision 6 left per-user provider keys out until they "become a real ask", and
  said they would arrive as member-plane routes that write owner-scoped rows.
- Git acts as the one account behind `GH_TOKEN` (decision 63's amendment).
- The desktop attaches only with a pasted token, which OIDC users do not have.

Gateway-authenticated machines already give each person their own identity
(decisions 49, 63, 65, 71, and 90). This record gives a standalone machine the
same result with parts it runs itself, through those records' seams. It keeps
decision 6's assumption: one small team of colleagues, not adversarial tenants.

## Decision

Isolation comes first: the machine offers no personal credential until members'
processes run apart from the server and from each other.

### 1. Each principal's processes run under its own account

Every principal that runs work, person or service, gets an operating-system
account id from a range the operator sets, inside the first 65,536 ids so that
rootless runtimes can map it. The server records each id and never reuses one,
so no member inherits a departed member's files.

The image gains a launcher, a small setuid-root program that runs a process as a
principal's account. It accepts requests only from the server's account, refuses
ids outside the range, and gives the process the principal's own group and no
other. That drops the host group that `group_add` gives the server to read
`tokens` and `secret.key`. The launcher stays the parent of what it starts, so
the server keeps its pipes, terminal, and exit status, and the launcher ends the
principal's process group when the server asks.

Everything the server does in a principal's worktrees and clones runs through
the launcher as that principal: terminals, setup scripts, quick actions,
engines, file writes, and the server's own `git` and `gh`. If the server ran git
itself, repository configuration such as `core.fsmonitor` would execute as the
server. Only reads stay with the server, which opens regular files beneath a
checkout without following links, as `code::scratch` does. Each principal's
`home`, `tmp`, and engine-private files live under
`{data_dir}/members/<owner segment>/`. Checkouts belong to the principal's
account and the server's group, and every child environment, terminals included,
uses the child allowlist. A session runs as its owner, whoever drives it
(decision 86).

`TIDEBREAK_MEMBER_ISOLATION` is `accounts` by default, or `off` for today's
shared account. At boot, a reserved probe account must fail to read each secret
file the server was given and to reach a mounted Docker socket. If that check
fails, or the platform forbids the privilege change, the server starts, logs
why, and refuses Code mode by name to members and to service principals, which
own Slack channel sessions. Administrators keep Code mode under the server's
account, since the deployment plane already lets them run commands on the host.
The stock stack needs nothing more, because Docker runs setuid programs by
default. One VM pays for a small program, a table of ids, and per-principal
directories and caches, with no daemon or container per member.

### 2. Members keep their own credentials

Each owner's credentials are one bundle item in the deployment's configured
custody, beside the deployment's bundle (decision 56). In decision 102's
database custody the row name carries the owner, so a row copied under another
owner's name does not decrypt. Code reaches the items only through a view bound
to the request's principal, as `ScopedStore` does for data.

The member plane gains `/me/` routes that set, delete, and report the caller's
own provider keys, engine sign-ins, GitHub connection, and desktop devices. The
deployment plane keeps the shared keys and a setting, on by default, that lets
members use them. Members store keys, never endpoints, so no member can aim the
server's requests at an address of their choosing.

For each provider, a member's own key wins, then the shared key while it is
shared, then no route. The sharing setting never cuts off administrators or
service principals, and a service principal holds no keys of its own
(decision 89). A machine offers member credentials only while no member process
runs as the server, so under `off` the `/me/` credential routes refuse.

### 3. Each person signs in to engines as themselves

An engine runs with `HOME` set to its owner's `home`, which only the owner's
account can open, and without the server's `CODEX_HOME`, `GH_CONFIG_DIR`, and
`XDG_*`. The server observes sign-in by running the engine's status command as
the owner (decision 34). A `/me/` route runs the pinned binary's own sign-in as
the caller, and the deployment-plane sign-in remains only under `off`. An engine
session picks its model account at start and shows it: the owner's engine
sign-in, else the owner's key for that provider, else the shared key, else a
refusal that asks the owner to sign in.

Keys reach an engine only through decision 71's relay, which sends the chosen
account's current key with each request. A removed key fails the request instead
of switching accounts, and no key the machine holds enters a child's
environment. Sign-ins in the shared home serve no session under `accounts`,
because relaying one person's sign-in to others would broker another product's
session (decision 34). The doctor names them for removal.

### 4. Git acts as the member through a GitHub App

A standalone machine can name a GitHub App: its id, client id and secret, and a
mounted private-key file, checked like decision 102's key file. The operator
registers the App with the repository permissions delivery uses, expiring user
tokens, and `<TIDEBREAK_PUBLIC_URL>/auth/github/callback` as its callback. Its
installation sets which repositories the deployment reaches (decision 96).

A lender for the App implements `GitCredentialLender`. An `installation` borrow
mints a token for the one repository the operation touches (decision 63). A
`person` borrow uses the member's user token, whose refresh token the machine
keeps after the member connects once from `/me/`. An unconnected member's probe
answers `NotConnected` with the machine's connect URL. The rest of decisions 63,
65, and 90 applies unchanged, so commits and pull requests name the member and
the UI says who acts. Without an App, the `GH_TOKEN` lender stays and the UI
names its account. Configuring both is a boot error.

### 5. The desktop attaches with the deployment's single sign-on

On an OIDC machine, the desktop's Connect dialog offers the identity provider in
place of token paste. The desktop runs decision 87's flow in the system browser
with a PKCE challenge and a loopback redirect, the only redirect the machine
accepts from it. The machine hands off as it does for decision 82's page, once,
by redirect, but to the loopback address and with a one-time code that the
desktop redeems with its verifier.

The desktop receives an hour-long bearer and a device credential. The machine
stores only the device credential's hash, so a restart costs the desktop one
refresh, not a sign-in. It replaces the credential on every refresh and revokes
the whole chain when a spent one returns. Each refresh re-checks the person with
the provider's refresh token, kept as the member's credential. A refusal revokes
the device, and without that token a desktop sign-in lasts one bearer. OIDC
still names only members, and members revoke their own devices from `/me/`.

### 6. Upgrades and what stays shared

After an upgrade, a deployment runs under `accounts`, and each owner's checkouts
pass to their account before the owner's first process runs. Members stay on the
shared keys, which engines now reach through the relay, until they add their own
or sharing stops. Sessions that relied on the shared engine sign-in use a shared
key or ask their owner to sign in. `GH_TOKEN` and token paste stay.

Shared on purpose: providers, endpoints, model roles, MCP servers, plugins,
connected apps, engine versions, worktree roots, the GitHub App, the shared
keys, and service principals' credentials. Excluded: per-member MCP servers,
plugins, and endpoints; network isolation between members; an identity-provider
claim that grants the admin role; forges other than GitHub; and the mobile
client, which keeps decision 98's refusal of OIDC machines.

### 7. Slices

Each slice is one pull request, in this order. Slices 1 to 3 close #3590.

1. The launcher and account ids, unused. Tests: ids are stable per owner and
   never reused; the launcher refuses another caller, an out-of-range id, and
   root; the probe reports the fallback under `no-new-privileges`.
2. One runner for every process and write that code mode makes in a checkout,
   still as the server. Test: a source contract test fails on any other spawn.
3. Isolation on, checkouts handed over, and the #3590 caveat removed from the
   guide and decision 102. Tests: the image test under Validation, and a turn
   from each pinned engine on a read-only install as a member.
4. Member credentials. Tests: two members' keys stay apart at the route and
   store layers, and a copied row fails to decrypt.
5. The relay on standalone machines. Tests: no machine key in a child's
   environment; A's turns carry A's key and B's the shared key; a session keeps
   its account when its owner adds a key mid-session.
6. Engine sign-in per person. Test: A's sign-in leaves B's session signed out.
7. The GitHub App lender. Tests: an installation mint names one repository; B's
   session cannot borrow A's token; a refresh whose replacement cannot be stored
   disconnects the member instead of reusing the spent token.
8. Desktop sign-on. Tests: a code redeems once, only with its verifier; a reused
   device credential revokes its chain; a restart keeps it attached.

## Alternatives Considered

**Per-member containers through the container backend.** A container could
confine a member's network with the sandbox-agent tier's egress proxy, which
accounts cannot. Rejected for now: the server would need a runtime socket, which
gives it root on the host, or a rootless runtime the operator runs. Each member
would need a running container that carries the engines, and every server
operation on a member's checkouts would cross into it. Slice 2's runner is where
containers would plug in later. These options were also rejected:

- Only stripping deployment variables: the server's account still reads
  `tokens`, the key file, and the server's `/proc/<pid>/environ`.
- Code mode for administrators only: it is the fallback, but as the answer it
  takes Code mode from every member.
- A launcher daemon started as root: the container would start as root, and the
  server would lose its parent link to what the daemon runs.
- A separate store for member credentials: a second custody to configure and
  back up, for no added protection.
- A member's own key in their engine's environment: a prompt-injected engine
  reads it there, and the relay keeps it out.
- Personal access tokens for git: long-lived, unscoped secrets, with no
  installation to bound repository access (decision 96).
- Doing nothing: the trust warning stays, and nobody works as themselves.

## Consequences

With members off the server's account, a Docker socket mounted for the `exec`
backend no longer gives every Code mode member root on the host. The launcher is
setuid-root code, where a bug escalates privilege, so it stays small and has its
own tests. Accounts separate files and processes, not the network or the kernel:
a member reaches what the machine reaches without a credential, such as cloud
metadata. Administrators, and anyone holding the key file and the database, can
read members' credentials. A member's own sign-in or key pays for their
sessions.

A platform that sets `no-new-privileges` must allow the launcher or accept the
fallback. Person git needs a registered App, desktop sign-on needs provider
refresh tokens, and a backup of the data volume now holds engine sign-ins. The
boundary is real only on Linux with several accounts, so an image test proves
it, and every server path into a member's checkouts needs two-account tests.

Revisit when members need network or resource isolation from each other, when
common platforms cannot run the launcher, or for a forge other than GitHub.

## Open questions for the owner before slice 1

1. Should upgrades default to `accounts`, refusing members' Code mode where the
   launcher cannot run, or stay `off` until the operator opts in?
2. Decision 49's machines run under a platform-chosen account. Does `accounts`
   apply there, and does their platform allow the launcher?
3. Is a setuid-root program acceptable in the published image, or should the
   launcher ship in an image variant that operators choose?
4. Without a provider refresh token, is a one-hour desktop sign-in right, or may
   it last a fixed number of days?
5. When a person leaves, should the machine delete their account, home, and
   credentials, or wait for an administrator?

## Validation

- Slice 3's image test runs two members. From one member's terminal and engine,
  reads of `tokens`, the key file, the server's `/proc/<pid>/environ`, home, and
  blobs, and the other member's checkouts, home, and relay key all fail.
  Stripping variables alone passes an `env` check and fails here.
- A member process holds only its own group. A launcher that skips `setgroups`
  passes every uid check, then reads `tokens` and `secret.key` through the host
  group.
- A member repository's `core.fsmonitor` command writes a marker. After the
  server's status, diff, checkpoint, and push, the marker is absent or owned by
  the member. A server that runs git as itself and silences git's ownership
  check with `safe.directory` passes every functional test and fails this one.
- Interrupt, reap, and terminal close leave no process of that session behind. A
  launcher that ends only its direct child leaves a grandchild running.
