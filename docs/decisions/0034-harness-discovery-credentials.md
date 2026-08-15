# 34. Harness Discovery and the Credential Boundary

- Status: Accepted
- Date: 2026-08-15
- Owners: code mode
- Related: [`0031-harness-adapter-boundary.md`](0031-harness-adapter-boundary.md),
  [`0033-code-mode-approvals.md`](0033-code-mode-approvals.md),
  [`docs/code-mode.md`](../code-mode.md)

## Context

The coding harnesses code mode drives are products the user already has a
relationship with: they installed them, signed into them, and pay for them
through their own subscriptions or API keys. Some users route them through a
managed model gateway; from the harness binary's point of view that is just
configuration — a correctly configured `claude` works identically whether its
traffic goes direct or through a gateway.

Tidebreak's credential story is strict and simple: API keys live in the OS
keychain, and Tidebreak never carries credential shapes beyond that. Becoming
a broker for *other products'* credentials would be a new category of
liability: their token formats, their refresh flows, their billing identity.

There is a real platform wrinkle: GUI applications on macOS do not inherit
the user's shell `PATH`. A harness installed via a shell profile
(`~/.zshrc` PATH additions, version managers) is invisible to a naive
`Command::new("claude")` from the app. This, not philosophy, is why discovery
needs a decision.

## Decision

**Discovery: the user's shell resolves the binary, in interactive login
mode.** Harnesses are found by asking the user's own shell with both login
and interactive semantics (`$SHELL -ilc 'command -v <binary>'`), because the
two profile classes split the configuration this exists to see: zsh sources
`.zprofile`/`.zlogin` for a login shell but `.zshrc` only when interactive —
and version managers and gateway configuration commonly live in `.zshrc`.
A login-only invocation would reproduce the GUI-PATH bug for exactly the
users it was meant to fix. The probe is bounded (timeout, output delimited
by sentinel markers so profile noise cannot forge a result) and cached;
re-probe is on demand from the doctor surface. Relative path results are
rejected; only absolute, executable paths are accepted.

**Execution environment: the probed shell's environment, minus Tidebreak
internals.** The same probe captures the shell's resolved environment once,
and harness children run under that snapshot — not the GUI process's
environment, which never saw the user's profiles — minus Tidebreak-internal
variables (anything Tidebreak-prefixed, and the server's own tokens). This
capture is the entire gateway story: a gateway-configured harness works
unchanged because its configuration — whatever env or config files it uses —
is the user's, untouched. Tidebreak adds only what the adapter contract
requires (working directory, the approval-channel wiring of
[`0033`](0033-code-mode-approvals.md), and non-secret marker variables).

**Credentials are observed, never brokered.** Tidebreak never reads, stores,
proxies, or injects harness credentials. Each adapter implements an
authentication *observation* — the cheapest command that distinguishes
signed-in from signed-out — and the result is reported, with remediation
copy that sends the user to the harness's own login flow in their terminal.

**The doctor surface.** A settings section and a `GET /code/harnesses` route
report, per harness: found or not, resolved path, detected version, adapter
tier, capability summary, authentication status, and remediation text. The
doctor is also where unrecognized-event counters
([`0031`](0031-harness-adapter-boundary.md)) surface per harness.

**Advanced launch configuration, bounded.** Settings may carry per-harness
extra arguments and environment additions for unusual setups. The composed
launch plan still passes the bypass-flag denylist of
[`0033`](0033-code-mode-approvals.md), and extra environment cannot override
the variables the adapter contract sets.

**No bundling in v1.** Tidebreak does not install, pin, or update harness
binaries. The existing pinned-runtime install pattern
(`crates/tidebreak-desktop/src/node_install.rs`) is the recorded escape
hatch if version drift becomes a support burden.

Deliberately excluded: Windows discovery (login-shell semantics differ;
Windows packaging is parked per [`docs/deferred.md`](../deferred.md)), and
any Tidebreak-mediated harness sign-in flow.

## Alternatives Considered

**Bundle pinned harness binaries.** Rejected: Tidebreak would take on each
harness's release cadence and, worse, its credential storage — a bundled
binary does not share the user's existing login. The drift-pain escape hatch
is recorded instead.

**API-key passthrough from Tidebreak's keychain** (store an Anthropic key and
inject it into Claude Code). Rejected: it makes Tidebreak a credential broker
for a third-party product, forks billing identity from the user's existing
subscription, and silently changes which account pays for a session.

**Static PATH resolution from the GUI process.** Rejected on the macOS fact
above: it would work in dev shells and fail for real users, the worst kind of
bug.

**A Tidebreak-owned gateway integration for harnesses** (Tidebreak configures
the harness to use a gateway). Rejected for v1: passthrough already serves
gateway users, and writing other products' config files violates the
observation-only boundary.

**Do nothing** (require the user to type absolute paths). Rejected: the
doctor surface with login-shell resolution is strictly better and cheap.

## Consequences

Tidebreak's behavior depends on the user's shell environment, which is the
point — but it means a broken shell profile breaks discovery, so probe
failures must render as diagnosable output (the shell's stderr, bounded) in
the doctor, not as generic errors.

Authentication observation is per-adapter maintenance: a harness that changes
its auth-status command breaks a probe, visibly, in the doctor.

Because credentials are never brokered, a session's spend lands on the
user's existing harness account, and Tidebreak can make no usage-accounting
promises beyond what the harness reports in its own stream.

Revisit this decision if harness version drift becomes a recurring support
burden (bundling escape hatch), or if a managed-deployment story requires
Tidebreak to *verify* rather than merely observe harness configuration.

## Validation

- Probe tests with shim binaries on a temporary PATH: missing binary,
  present-but-unauthenticated, version-string variants, relative-path
  rejection, and a shell profile that prints garbage before the answer
  (the sentinel markers must survive it).
- An environment-capture test: a variable set only in the shell's
  interactive profile reaches the spawned child, proving the child env is
  the shell snapshot and not the GUI process environment; and the snapshot
  contains no Tidebreak-prefixed secret variables and no keychain-derived
  values while preserving arbitrary user variables (the
  gateway-passthrough property).
- Doctor route tests for each degraded state, including bounded stderr
  passthrough on probe failure.
- The composed-argv denylist test (shared with
  [`0033`](0033-code-mode-approvals.md)) including settings-supplied extras.
- A plausible wrong implementation resolves the binary with the GUI
  process's own PATH and passes on developer machines; the probe tests must
  run resolution through the shim shell to fail it.
