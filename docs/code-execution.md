# Code execution

OpenWave exposes one foreground `exec` tool through a provider-neutral command
execution contract. The first provider is a native local sandbox. A managed
provider can implement the same contract without changing the model-facing tool
schema; E2B and Daytona are the current managed adapters.

This capability is separate from a sandbox *agent*. A sandbox agent is a
depth-one model run with a constrained tool budget. A code-execution sandbox is
one bounded process invocation owned by the foreground coordinator.

## Configuration

The authenticated local API owns provider selection and timeout policy:

| Route | Purpose |
| --- | --- |
| `GET /code-execution` | Return the selected provider, timeout, egress policy, and host readiness |
| `PUT /code-execution` | Select a fixed provider or disable execution, update the bounded timeout, and set the managed-sandbox egress policy |
| `GET /code-execution/credentials` | Read readiness for the fixed E2B and Daytona key slots |
| `PUT /code-execution/credentials/{e2b\|daytona}` | Store that provider's API key in its fixed host-secret slot |
| `DELETE /code-execution/credentials/{e2b\|daytona}` | Remove only that provider's saved API key |

The initial state is:

```json
{
  "provider": "local",
  "timeout_ms": 20000,
  "available": true,
  "has_credential": false
}
```

`available` reports whether the selected native confinement primitive exists on
the current host. The example above is the supported macOS state; it is false
when execution is disabled or unsupported.
Timeouts must be between 1 and 120 seconds. Sending `{"provider": null}`
disables execution; sending `{"provider": "local"}` enables the local adapter.
`e2b` and `daytona` select the managed adapters, which become available once
their fixed credential slot is populated. No executable, endpoint, environment
value, or secret reference is accepted by the non-secret settings surface.

`GET /code-execution` reports `has_credential` for the selected provider only,
while `GET /code-execution/credentials` reports readiness for both managed slots
independently. Local execution needs no credential and has no slot to report.

### Egress policy

`GET`/`PUT /code-execution` also carry a host-owned, non-secret egress policy for
the managed sandboxes, under the `egress` field. It is a store setting, not a
keychain secret: it holds only an allowlist of domain patterns and CIDR blocks,
or `open`, and the surface accepts no endpoint or secret. The model never sets
it — it is host configuration, like provider selection and timeout.

The `policy` is one of:

```json
{ "mode": "open" }
{ "mode": "allowlist", "domains": ["*.pypi.org"], "cidrs": ["140.82.112.0/20"] }
```

Egress restriction is **opt-in, and the default is `open`**. Managed sandboxes
have always been created with open internet access, and flipping the default to
deny would break package installs and network fetches inside code execution, so
`open` stays the out-of-the-box behavior and is disclosed as such in settings.
Configuring an allowlist switches every managed sandbox created afterwards to
deny-by-default: only the listed domains and address blocks are reachable, and
an empty allowlist denies all egress. Domain and address rules are independent —
a domain grant never opens a raw IP and an address block never opens a
hostname — matching the decision layer in
[sandbox providers](sandbox-providers.md). Each pattern is validated to that
layer's grammar at `PUT` time, so a malformed grant is a bad request rather than
a silent widening; a malformed *stored* policy fails closed by refusing
execution, never by reverting to open egress. The local provider already denies
all network and is unaffected.

The `enforcement` field discloses, per managed provider, whether its egress
restriction is confirmed against the live vendor API:

- **E2B is confirmed.** A configured allowlist becomes E2B's per-sandbox
  `allowOut` rules with `allow_internet_access: false`. DNS resolution and
  non-HTTP/S ports stay reachable regardless of policy, as E2B's mechanism
  allows.
- **Daytona is pending.** A configured policy is sent at sandbox creation
  (block-all switch or comma-separated allowlists), but whether an
  empty-but-present allowlist denies that axis is not yet confirmed against the
  live Daytona API, and Daytona keeps general-purpose services (package
  registries, git hosting, container registries, AI APIs) reachable regardless
  of policy. Daytona egress is therefore applied but must not be relied on as a
  network boundary until confirmed.

The `exec` tool remains registered with a stable schema while settings change.
The host resolves the selected provider immediately before execution, so a
configuration update takes effect without restarting OpenWave.

## Desktop setup

The desktop sidebar's **Code execution** panel drives this same local API. It
offers a key field per managed slot, so E2B and Daytona can both hold a key and
switching between them needs no retyping, and a separate choice of which
provider agents execute in — the local sandbox, one of the managed ones, or
disabled. Saving writes every key the user typed before it writes selection, so
a provider cannot become active in a pass that failed to store its key. Saved
keys are never displayed or read into the renderer.

## Provider contract

`openwave-code-execution` owns the normalized boundary:

```text
ExecTool
    |
    v
CodeExecutionProvider::execute
    |
    +-- LocalExecutionProvider
    |
    +-- E2BExecutionProvider --------+
    |                                |
    `-- DaytonaExecutionProvider ----+-- shared remote session + receipt layer
```

A request contains:

- a stable execution ID derived from the canonical tool call;
- an opaque workspace ID derived from the chat;
- one executable and argument vector (not an implicitly parsed shell string);
- one private-workspace-relative current directory.

The execution ID is a provider idempotency key. Reusing it with different
arguments is an identity conflict. The workspace ID lets local execution map a
chat to private scratch and lets managed providers map the same chat identity to
a reusable remote sandbox.

Every provider returns the same bounded shape: provider kind, optional exit
code, stdout, stderr, timeout and truncation flags, and duration. Provider-native
responses, credentials, absolute host paths, and unbounded logs do not cross
the contract.

## Workspace lifecycle

Beside `execute`, a provider may offer an optional durable-workspace
capability: create/connect/destroy a chat's workspace, put one file, get one
file, and list one directory. The capability is flagged — a provider that has
no durable session reports none, and callers degrade instead of failing. It is
host-internal only: no model-facing tool is registered over it, and gating any
model-facing surface is a separate step in the
[sandbox providers](sandbox-providers.md) delivery sequence.

The same rules as `execute` bound the surface. Paths are workspace-relative
with only normal components (no traversal, no absolute host paths), file
transfers are capped in both directions, listings are capped with an explicit
truncation flag, and errors are normalized — a missing file, an oversized
transfer, and an unreachable backend are distinct outcomes. No credential or
provider-native response crosses the contract.

The local provider implements it directly over private per-chat scratch,
rejecting symlinked files and symlinked intermediate directories, and offers
it even on hosts where the native confinement primitive for `execute` is
unavailable, because managing scratch files executes nothing. E2B and Daytona
implement it over their session and toolbox file APIs through the same shared
remote-session layer as commands, so file operations serialize with command
execution per chat and reconcile the remote sandbox first. Connect reports
reachable only for a sandbox the host holds a live handle to; destroy releases
the handle only after the backend acknowledges, so a failed teardown stays
retryable.

## Local native sandbox

The initial adapter is deliberately fail-closed and macOS-first:

- `/usr/bin/sandbox-exec` applies a generated Seatbelt profile;
- network is denied;
- writes are allowed only below the exact canonical private chat scratch;
- sensitive user, application, configuration, temporary, and volume paths are
  denied for reads, while system executables and runtime libraries remain
  usable;
- the parent environment is cleared; only fixed `HOME`, `TMPDIR`, and `PATH`
  values are supplied;
- stdin is `/dev/null`;
- the process runs in its own process group;
- wall time, CPU time, open files, per-file writes, arguments, and captured
  output are bounded;
- timeout terminates and then kills the process group;
- unsupported platforms return unavailable and never fall back to an
  unconfined process.

This is a defense-in-depth boundary for OpenWave's single-user local runtime,
not a VM-grade multi-tenant boundary. Hostile or remotely supplied workloads
should use a managed isolation provider.

The executable receives its arguments directly. A model that truly needs a
shell must invoke one explicitly, such as `/bin/sh` with `["-c", "..."]`.

Before spawning, the adapter durably creates a private `running` receipt keyed
by the stable execution ID and request fingerprint. It atomically replaces that
marker with the bounded terminal response. An exact retry returns the cached
response; a changed request is rejected; a surviving `running` marker is
reported as ambiguous and is not replayed. Receipts live outside every
model-visible chat scratch directory.

`exec` is still classified `Sensitive` and crosses the existing durable
approval/standing-grant boundary. Native confinement limits what an approved
command can do; it does not replace user consent for command execution.

## Managed sandboxes

E2B and Daytona share one process-local session pool, request fingerprint and
receipt state machine, bounded capture primitive, response decoder, and
credential primitive. Both serialize commands within a chat workspace and
reconcile the remote sandbox before each new execution. An exact retry returns
the cached normalized response; a changed request is rejected; an ambiguous
dispatch is never started a second time.

The provider adapters own only their control-plane and command transports:

- E2B sends the executable and argv directly through envd's process protocol.
- Daytona's toolbox accepts shell text, so its adapter quotes every executable
  and argv element before dispatch and prefixes the result with `exec`. Shell
  metacharacters therefore remain argument data. A caller that deliberately
  needs a shell must still name `/bin/sh` and `-c` explicitly.

Managed credentials remain in the OS secret store and never enter configuration,
tool arguments, logs, or renderer responses. Remote API endpoints are fixed by
the build; Daytona toolbox URLs returned by the control plane are restricted to
HTTPS Daytona origins. By default both managed providers allow internet access
inside the sandbox, unlike the local native provider; a configured
[egress policy](#egress-policy) restricts that access at sandbox creation, with
E2B's enforcement confirmed and Daytona's pending live confirmation.
