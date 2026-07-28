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
| `GET /code-execution` | Return the selected provider, timeout, and host readiness |
| `PUT /code-execution` | Select a fixed provider or disable execution, and update the bounded timeout |
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

The `exec` tool remains registered with a stable schema while settings change.
The host resolves the selected provider immediately before execution, so a
configuration update takes effect without restarting OpenWave.

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
HTTPS Daytona origins. Both managed providers allow internet access inside the
sandbox, unlike the local native provider.
