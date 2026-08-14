# Code execution

Tidebreak exposes one foreground `exec` tool through a provider-neutral command
execution contract. The first provider is a native local sandbox. Another
provider can implement the same contract without changing the model-facing tool
schema: E2B and Daytona are the managed cloud adapters, and Docker runs the same
image in a container on the host's own runtime.

This capability is separate from a sandbox *agent*. A sandbox agent is a
depth-one model run supervised by a step-based check-in cadence. A code-execution sandbox is
one bounded process invocation owned by the foreground coordinator.

## Configuration

The authenticated local API owns provider selection and timeout policy:

| Route | Purpose |
| --- | --- |
| `GET /code-execution` | Return the selected provider, timeout, provider-enforcement disclosure, and host readiness |
| `PUT /code-execution` | Select a fixed provider or disable execution and update the bounded timeout |
| `GET /code-execution/credentials` | Read readiness for the fixed E2B and Daytona key slots |
| `PUT /code-execution/credentials/{e2b\|daytona}` | Store that provider's API key in its fixed host-secret slot |
| `DELETE /code-execution/credentials/{e2b\|daytona}` | Remove only that provider's saved API key |

The initial state is:

```json
{
  "provider": "local",
  "timeout_ms": 60000,
  "available": true,
  "has_credential": false
}
```

`available` reports whether the selected native confinement primitive exists on
the current host. The example above is the supported macOS state; it is false
when execution is disabled or unsupported.
Timeouts must be between 1 and 120 seconds; the default is 60 seconds, enough headroom for a cold package install that pulls compiled wheels. Sending `{"provider": null}`
disables execution; sending `{"provider": "local"}` enables the local adapter.
`e2b` and `daytona` select the managed adapters, which become available once
their fixed credential slot is populated. `docker` selects the container
adapter, which needs no credential and becomes available once a
Docker-compatible runtime is installed and its daemon answers. No executable, endpoint, environment
value, or secret reference is accepted by the non-secret settings surface.

`GET /code-execution` reports `has_credential` for the selected provider only,
while `GET /code-execution/credentials` reports readiness for both managed slots
independently. Local execution needs no credential and has no slot to report.

### Per-chat network policy

Every chat persists one provider-neutral code-execution policy. It defaults to
`off`, is selected beside the composer, and is read again immediately before
each command:

```json
{ "mode": "off" }
{ "mode": "package_managers" }
{
  "mode": "allowed_hosts",
  "allowed_hosts": ["api.example.com"],
  "package_managers": true
}
{ "mode": "open" }
```

Custom hosts are exact DNS names: wildcard patterns and address literals are
rejected, entries are lowercased and deduplicated before persistence, and the
list is bounded. `package_managers` expands to a fixed, reviewable registry
class (PyPI, npm, crates.io, Maven, Go, NuGet, RubyGems, and Packagist
endpoints). The model cannot author or widen the policy.

The local adapter starts an execution-scoped HTTP CONNECT broker on
`127.0.0.1` for every non-`off` command. Seatbelt admits outbound TCP to that
one exact port and nothing else; the child receives upper- and lower-case proxy
environment variables. The broker accepts CONNECT only, checks the requested
host against the chat policy, resolves it outside the sandbox, and rejects the
whole name if any answer is loopback, RFC1918/unique-local, link-local,
multicast, or otherwise non-routable. A denial returns an HTTP error
immediately rather than dropping packets and triggering package-manager retry
backoff. TLS remains end to end: the broker does not intercept or rewrite
requests. Dropping the command-scoped broker closes its listener and tunnels.

E2B and Daytona compile the same chat policy into their per-sandbox controls.
The older host-level `egress` value remains readable for stored-config
compatibility and provider disclosure, but chat execution always uses the
per-chat policy.

The `enforcement` field discloses, per non-native provider, an honest `status`
(`boundary`, `conditional_boundary`, `applied_with_gaps`, `unconfirmed`, or
`not_enforced`),
plus the `gaps` the vendor leaves reachable regardless of policy and an optional
`requirement` when a boundary is gated on a precondition. The status is
**derived from the shipped enforcement model** (`EgressEnforcement`), not
asserted per provider, so the settings surface and the decision layer can never
disagree — if the model says a vendor's mechanism leaves a general-purpose
destination reachable, the surface cannot present it as a full boundary.

- **E2B — `applied_with_gaps`.** A configured allowlist becomes E2B's
  per-sandbox `allowOut` rules with `allow_internet_access: false`, and this is
  confirmed against the live API. It is **not a full boundary**: domain rules
  are enforced only on ports 80/443 and DNS resolution stays open, so code in
  the sandbox can still reach arbitrary hosts on other ports or tunnel over DNS.
  The model's `is_credential_boundary()` returns false for exactly this reason,
  and the projection reads that value rather than a hardcoded flag.
- **Daytona — `conditional_boundary`, requiring org tier 3+.** A configured
  policy is sent at sandbox creation (block-all switch or comma-separated
  allowlists). A live test against a real Daytona account confirmed that a
  per-sandbox policy is a *strict*, externally enforced allowlist: only listed
  domains are reachable, and raw IPs, unlisted domains, unlisted-domain DNS, and
  the package registries / git hosting / container registries / AI APIs that
  were once assumed always-reachable are **all blocked**. There is no
  general-purpose carve-out, so `is_credential_boundary()` returns true and
  Daytona is in fact a stronger boundary than E2B. The one caveat is a
  precondition the host cannot read statically: the per-sandbox egress override
  requires **Daytona org tier 3+**. On tier 1–2 the override is refused and the
  org default applies, so the boundary is not guaranteed — the projection
  therefore reports it as a *conditional* boundary with that requirement inline,
  never an unconditional green one.

- **Docker — `boundary` for "no network", `not_enforced` for every other
  class.** The container backend can enforce exactly one policy class as
  written. A policy that permits nothing — what a conversation set to "no
  network" compiles to — creates the container with `--network none`, so it
  has no interface but loopback: no route, no name resolution, nothing to
  negotiate with. That is externally enforced by the runtime with no exception
  left open, so the row is derived from an `EgressEnforcement` declaration
  like the vendors', not asserted. An allowlist ("package managers only",
  custom hosts) cannot be honored yet: compiling it onto the runtime's default
  network would treat it as the open internet (LAN, `host.docker.internal`,
  cloud metadata). Those policies therefore also create the container with
  `--network none` — a refusal of the grants, not enforcement of them — and
  the row stays `not_enforced` so the surface cannot read them as a working
  restriction. That is distinct from `unconfirmed`, which describes a policy
  that *was* sent. Only open egress (no policy) leaves the container on the
  runtime's default network. Honoring the remaining classes needs the
  per-container internal network plus egress proxy the sandbox-agent container
  tier already runs; that is a later slice.

Because the container row depends on the policy and not only on the backend,
the settings surface renders it for the host-level `egress` value it displays.
Chat execution compiles the per-chat policy, so a chat set to "no network"
gets a no-network container even while the host-level row reads
`not_enforced` for an open default.

The local adapter is an unconditional external boundary: direct networking
stays denied and the only pinhole reaches the policy broker. Managed fidelity
is disclosed honestly as described above.

The `exec` tool remains registered with a stable schema while settings change.
The host resolves the selected provider immediately before execution, so a
configuration update takes effect without restarting Tidebreak.

## Desktop setup

The desktop sidebar's **Code execution** panel drives this same local API. It
offers a key field per managed slot, so E2B and Daytona can both hold a key and
switching between them needs no retyping, and a separate choice of which
provider agents execute in — the local sandbox, one of the managed ones, or
disabled. Saving writes every key the user typed before it writes selection, so
a provider cannot become active in a pass that failed to store its key. Saved
keys are never displayed or read into the renderer. Network policy is not a
provider setting; the conversation composer owns it so switching providers does
not silently change a chat's authority.

## Provider contract

`tidebreak-code-execution` owns the normalized boundary:

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
    +-- DaytonaExecutionProvider ----+-- shared remote session + receipt layer
    |                                |
    `-- DockerExecutionProvider -----+
```

A request contains:

- a stable execution ID derived from the canonical tool call;
- an opaque workspace ID derived from the chat;
- one executable and argument vector (not an implicitly parsed shell string);
- one private-workspace-relative current directory;
- an optional bounded list of scratch-relative files or directories to stage
  into a managed sandbox before the command runs;
- for native local execution only, a bounded set of absolute folder paths
  resolved by the host from the chat's current root attachments.

The execution ID is a provider idempotency key. Reusing it with different
arguments is an identity conflict. The workspace ID lets local execution map a
chat to private scratch and lets managed providers map the same chat identity to
a reusable remote sandbox.

Every provider returns the same bounded shape: provider kind, optional exit
code, stdout, stderr, timeout and truncation flags, and duration. Provider-native
responses, credentials, and unbounded logs do not cross the contract. Absolute
folder paths are a host-only input to the local adapter: they are never tool
arguments and are stripped from managed-provider requests.

## Container execution

The Docker backend is opt-in and never a default. It exists because the native
local sandbox is macOS-only: without it, a Linux or Windows host's only options
upload the conversation's staged files to a vendor.

It runs the same digest-pinned documents image the Daytona adapter registers as
a snapshot and the E2B template is built from, so LibreOffice, the document
skills' preinstalled Python dependencies, Node with the deck library, and the
bundled exec helpers are present at the same versions on every backend. The pin
lives in one module (`sandbox_image.rs`) that both backends in the crate read,
and the image-publish workflow rewrites it there.

One container serves one chat workspace, under a name derived from the
workspace id so a restarted host adopts its containers rather than duplicating
them. Commands cross as an argument vector through `docker exec` under an
in-container `timeout`, so a command that exceeds its limit is stopped rather
than left running behind an abandoned CLI. Workspace file transfers use the
same channel. The container's only process is a bounded sleep and it is created
with `--rm`, so an abandoned chat's container and its workspace volume remove
themselves.

Confinement is the container itself: the image's unprivileged uid forced from
the host, every Linux capability dropped, privilege escalation refused, and
process, memory, and CPU ceilings. No host path is bind-mounted — the workspace
is an anonymous volume, and host files enter only through the staging the host
performs for the paths a call listed. The root filesystem is deliberately
writable, unlike the sandbox-agent container tier: foreground exec installs
packages and writes scratch, and the surface a read-only root would protect is
the container's own ephemeral layer.

The chat's network policy reaches container creation, but only its strictest
class is enforced there as written: "no network" creates the container with
`--network none`. Every other *restrictive* class also creates the container
with `--network none` rather than the runtime's default network — a
fail-closed refusal, not a working allowlist — and only open egress leaves
the default network in place. See the enforcement disclosure above. The
container a chat is using is bound to the policy it was created under, on two
axes. The pooled session records the policy's fingerprint, so editing the
policy destroys the container and creates a replacement instead of reusing
one with stale networking. The configuration label a container carries
records its network shape, so a container found by its deterministic name —
after a host restart, or from a second window, where no pooled handle exists
— is replaced rather than adopted when its networking does not match.

## Connected folders in local exec

On macOS, the configured server intersects the chat's product attachment IDs
with the host broker's live read and write grants immediately before each
`exec` invocation. The local adapter rejects missing roots and roots presented
as symlinks, canonicalizes every path, and adds one narrow Seatbelt `subpath`
read allowance per readable root. A write allowance is added only when the
live grant is write-scoped. The profile's existing network denial and broad
user-data read denials remain in place.

Folder paths and access modes are listed in the foreground operating context so
the model can address them without inventing paths. That list is bounded and is
guidance rather than authority: the broker and profile are resolved again for
every invocation. Revocation therefore applies to the next command. A process
already running under a compiled profile keeps that access until it exits.

Local host-folder grants are currently macOS-only. Other local targets retain
their existing behavior, and E2B or Daytona cannot access host folders.

## Document helpers

Desktop builds ship a network-free Python helper library into every exec
workspace at `.tidebreak/exec-scripts`. The sandbox container includes the same
library at `/opt/tidebreak/exec-scripts`, exposed through
`TIDEBREAK_EXEC_SCRIPTS`. The helpers print short summaries and write visual
review images into `preview/`, using priority names such as
`overview-grid.png` before per-page or per-sheet images.

Examples:

```text
python3 .tidebreak/exec-scripts/render_pdf.py documents/report.pdf --pages 1-2
python3 .tidebreak/exec-scripts/extract_pdf_figures.py documents/report.pdf
python3 .tidebreak/exec-scripts/analyze_xlsx.py documents/model.xlsx
python3 .tidebreak/exec-scripts/calc_uno.py set-cell documents/model.xlsx Summary B7 '=SUM(B2:B6)'
python3 .tidebreak/exec-scripts/xlsx_recalc.py output/model.xlsx
```

PDF rendering uses pypdfium2 or pdf2image with Poppler, figure extraction uses
Poppler, DOCX/PPTX rendering uses LibreOffice, and XLSX analysis uses openpyxl
and Pillow. Editing an existing spreadsheet in place and recalculating one both
drive headless LibreOffice Calc over its `uno` Python bridge, which exists in
the sandbox image and not on a bare host. The sandbox image includes these
tools. Local execution reports a
concise command error when the host lacks Python or an underlying renderer; it
does not download tooling or use an unconfined fallback.

The local backend warms its verified wheel cache one exact requirement set at
a time. A successful set, or one pip proves has no compatible distribution for
the fixed interpreter, is remembered for the process lifetime so later execs
do not repeat the same deterministic resolution and warning. Network, timeout,
and process-launch failures remain retryable, and any changed exact pin set
receives one fresh attempt.

Beyond the helpers, the sandbox image carries the runtimes a document run may
reach for directly: LibreOffice Writer, Calc, and Impress with the `uno` Python
bridge for driving a running LibreOffice from a script, and Node.js with
`pptxgenjs` installed globally, so `require("pptxgenjs")` resolves from any
directory without a local `npm install`.

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
  values are supplied, with `HOME` and `TMPDIR` pointing at a writable per-chat
  directory outside the model-visible scratch so interpreter caches never
  surface as chat files;
- stdin is `/dev/null`;
- the process runs in its own process group;
- wall time, CPU time, open files, per-file writes, arguments, and captured
  output are bounded;
- timeout terminates and then kills the process group;
- unsupported platforms return unavailable and never fall back to an
  unconfined process.

An absolute path beneath a host area that the local profile denies is reported
as `sandbox_path_denied`, not as a missing workspace file. Direct path
arguments are rejected before the process starts. When a shell or interpreter
embeds the path in a script, a failed Seatbelt access is annotated in the
bounded stderr result while preserving the original diagnostic. The message
names the path, identifies the permitted model-visible roots (the private chat
workspace plus any currently connected folders), and tells the caller to
attach or copy the file into scratch or connect its containing folder before
retrying. A relative path that is absent inside scratch, or an absent path
beneath a connected folder, remains an ordinary `ENOENT`; callers can therefore
recover from the two cases without blindly retrying the same inaccessible host
path.

This is a defense-in-depth boundary for Tidebreak's single-user local runtime,
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

### File staging

A managed sandbox has its own filesystem, so the host moves files between the
chat's private scratch and the sandbox around each command. The contract is
explicit, not a mirror:

- **Only listed paths go in.** The `exec` call's `files` argument names the
  scratch-relative files or directories (up to 64 paths) the command needs;
  directories stage recursively. Nothing else in scratch is uploaded — a file
  written with `write_file` or a materialized attachment under `documents/` is
  visible to a managed command only if listed. The host also stages a fixed
  set of infrastructure on its own: the hidden markers that make `output/` and
  `preview/` exist remotely, and the bundled document helpers under
  `.tidebreak/exec-scripts`.
- **Failures are loud, never silent.** A listed path that does not exist, is a
  symlink, or expands past the 256-file staging bound fails the call with an
  error naming the path. The local provider validates the listed paths the
  same way even though it stages nothing, so a bad path fails identically on
  every provider. Only per-entry conditions found *inside* a listed directory
  (a dependency tree, a nested symlink, an oversized file) degrade into
  bounded sync notes in the tool result. The per-file transfer cap applies in
  both directions.
- **Unchanged files are not re-uploaded.** The session pool remembers a
  content digest per staged path for the live sandbox instance and skips
  identical content on later commands in the same session. The memory is bound
  to the exact sandbox: a recreated or destroyed sandbox starts from an empty
  ledger and is staged again.
- **Only `output/` and `preview/` come back.** After the command, the host
  pulls those two subtrees into scratch — they feed the output-versioning and
  preview-image scans — validating every returned path and refusing anything
  the backend lists outside them. Intermediates the command wrote elsewhere
  stay in the sandbox for later commands in the same session.

When a managed command fails, the sync notes include one bounded `staged:`
line naming what was staged (or pointing at the `files` argument when nothing
was), so a missing-input failure is diagnosable from the tool result.

Managed credentials remain in the OS secret store and never enter configuration,
tool arguments, logs, or renderer responses. Remote API endpoints are fixed by
the build; Daytona toolbox URLs returned by the control plane are restricted to
HTTPS Daytona origins. By default both managed providers allow internet access
inside the sandbox, unlike the local native provider; a configured
[network policy](#per-chat-network-policy) restricts that access at sandbox creation. E2B's
enforcement is confirmed but applied-with-gaps; Daytona's per-sandbox policy is a
strict, live-confirmed boundary conditional on org tier 3+.
