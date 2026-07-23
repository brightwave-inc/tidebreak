# Code execution

OpenWave exposes one foreground `exec` tool through a provider-neutral command
execution contract. The first provider is a native local sandbox. A managed
provider such as E2B can implement the same contract later without changing the
model-facing tool schema.

This capability is separate from a sandbox *agent*. A sandbox agent is a
depth-one model run with a constrained tool budget. A code-execution sandbox is
one bounded process invocation owned by the foreground coordinator.

## Configuration

The authenticated local API owns provider selection and timeout policy:

| Route | Purpose |
| --- | --- |
| `GET /code-execution` | Return the selected provider, timeout, and host readiness |
| `PUT /code-execution` | Select a fixed provider or disable execution, and update the bounded timeout |

The initial state is:

```json
{
  "provider": "local",
  "timeout_ms": 20000,
  "available": true
}
```

`available` reports whether the selected native confinement primitive exists on
the current host. The example above is the supported macOS state; it is false
when execution is disabled or unsupported.
Timeouts must be between 1 and 120 seconds. Sending `{"provider": null}`
disables execution; sending `{"provider": "local"}` enables the local adapter.
No executable, endpoint, environment value, or secret reference is accepted by
this settings surface.

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
    `-- future managed provider (for example E2B)
```

A request contains:

- a stable execution ID derived from the canonical tool call;
- an opaque workspace ID derived from the chat;
- one executable and argument vector (not an implicitly parsed shell string);
- one private-workspace-relative current directory.

The execution ID is a provider idempotency key. Reusing it with different
arguments is an identity conflict. The workspace ID lets local execution map a
chat to private scratch and gives a future managed provider a stable key for a
remote session or staged workspace.

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
should use a managed isolation provider once one is available.

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

## Adding E2B or another managed provider

A managed adapter should preserve the same invariants:

1. Treat the execution ID as an idempotency/reconciliation key.
2. Map the opaque workspace ID to a bounded remote sandbox lifecycle.
3. Keep credentials and endpoint selection host-owned.
4. Enforce host-selected time, output, file, concurrency, and network policy.
5. Normalize terminal results before they enter model context.
6. Fail conservatively after an ambiguous dispatch instead of starting a second
   remote job.

Provider-specific configuration and credentials can be added beside the current
host-owned selection once a second adapter exists. They should not become
`exec` arguments.
