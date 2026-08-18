# 45. Run code mode on Windows

- Status: Proposed
- Date: 2026-08-18
- Owners: code mode and desktop
- Related: [decision 32](0032-code-workspaces-worktrees-checkpoints.md),
  [decision 34](0034-harness-discovery-credentials.md),
  [decision 44](0044-install-pinned-harnesses-on-linux.md),
  [`docs/code-mode.md`](../code-mode.md)
- Supersedes: [decision 44](0044-install-pinned-harnesses-on-linux.md)

## Context

Decision 44 made the desktop provision one digest-verified Node runtime and
made pinned harness probes and sessions use it on macOS and Linux. Windows
desktop packages now ship for x86_64 and ARM64, but code mode remains disabled
by several related Unix assumptions rather than by one capability gate.

Official Windows Node distributions are ZIP archives whose `node.exe` and
`npm.cmd` entrypoints live at the archive root. npm-installed harnesses expose
Windows command shims under `node_modules/.bin`. Tidebreak's existing managed
runtime verifier and harness launcher instead assume a `bin/node`, `bin/npm`,
and extensionless harness entrypoint.

The code-mode runtime also treats a POSIX login shell as the source of truth
for the user's environment and invokes setup and quick-action scripts with
`$SHELL -lc`. Windows desktop processes already receive the signed-in user's
environment from the operating system, while neither PowerShell profiles nor
`cmd.exe` startup files provide a stable machine-readable equivalent of
`env -0`. Finally, interrupting only the immediate process is insufficient on
Windows: launching a `.cmd` shim adds a command interpreter and Node child, and
leaving either descendant alive breaks cancellation and restart recovery.

Windows paths add a separate correctness boundary. Drive-letter paths are
case-insensitive, UNC paths are valid repository roots, and canonicalization
may add a verbatim path prefix. Persisted repository identities cannot depend
on spelling differences that Windows treats as equal.

## Decision

The desktop-managed Node installer supports the shipped Windows x86_64 and
ARM64 targets. It downloads the exact official ZIP for the existing Node pin,
verifies the platform-specific SHA-256 before extraction, rejects archive
entries that escape the staging directory, and installs the archive root under
`tools/node/<version>/`. The shared managed-runtime contract describes the
artifact digest and platform layout. Verification requires the platform's
actual entrypoints: root-level `node.exe` and `npm.cmd` on Windows, and
`bin/node` and `bin/npm` on macOS and Linux. No platform falls back to ambient
Node.

Pinned harness resolution is platform-aware. npm installation on Windows is
invoked through the managed `npm.cmd`; a harness resolves to its generated
`.cmd` shim, while Unix resolves the existing extensionless entrypoint. Every
probe and live session receives the managed runtime directory first on
`PATH`, and both paths retain marker-gated exact-version verification.

Windows environment capture uses the current desktop/server process
environment as the operating-system snapshot. It does not execute PowerShell
profiles to synthesize another environment. Environment-key updates are
case-insensitive so `Path` and `PATH` cannot become competing values. macOS and
Linux retain login-shell capture.

User-authored setup, archive, and quick-action scripts run with Windows
PowerShell using non-profile, non-interactive flags. Tidebreak passes script
text as one PowerShell command and does not reinterpret it as POSIX shell or
`cmd.exe` syntax. Unix retains `$SHELL -lc`.

Every spawned harness or user script is owned as a process tree. On Windows,
Tidebreak creates the root process suspended, assigns it to a kill-on-close Job
Object, and only then resumes it. Interruption, timeout, and owner drop
terminate the job, including `.cmd`, Node, and tool descendants. Adapters do
not carry independent Windows signal implementations. Unix retains the
existing interrupt-and-escalate behavior.

Repository validation continues to use canonical filesystem paths and
argv-based Git commands. Windows repository identity comparisons normalize
verbatim prefixes and compare case-insensitively without rejecting drive-letter
or UNC roots. Tidebreak still creates worktrees under its data directory and
does not introduce a user-selectable worktree location.

This decision enables pinned local code-mode harnesses on Windows. It does not
enable Tidebreak's native local execution sandbox, managed LibreOffice,
computer use, or Windows Authenticode signing.

## Alternatives Considered

### Require users to install Node and harnesses globally

Rejected. Ambient installs reintroduce version drift, make probes and sessions
resolve different executables, and weaken the exact artifact contract already
established on macOS and Linux.

### Use `cmd.exe /c` for every Windows command

Rejected. It is an implementation detail of npm-generated shims, but it is a
poor contract for user scripts and provides no stable environment-capture
protocol. PowerShell is the native user-script surface.

### Load PowerShell profiles and parse the resulting environment

Rejected. Profiles may prompt, print arbitrary data, mutate state, or never
return. The desktop process already receives the signed-in user's environment;
capturing it directly is deterministic and matches other native GUI software.

### Assign a normally running child to a Job Object after spawn

Rejected. A fast `.cmd` wrapper can create a Node descendant before assignment,
allowing it to escape cancellation. Suspended creation makes ownership atomic
from the child's perspective.

### Kill only the spawned child process

Rejected. npm shims and command interpreters create descendants. Killing only
the child Tidebreak directly spawned can leave Node and tool subprocesses
running against a supposedly interrupted worktree.

### Compare persisted Windows paths as raw strings

Rejected. Drive-letter case, separator spelling, and verbatim prefixes can
describe the same path. Raw-string identity would permit duplicate repository
registrations and make validation depend on presentation.

### Keep Windows code mode unavailable

Rejected once the native runtime, shell, process-tree, and path contracts are
implemented and covered by Windows CI. The structured harness protocol and
durable workspace model are otherwise platform-independent.

## Consequences

The managed Node contract gains explicit archive-layout and executable helpers
instead of treating every artifact as a Unix tarball. The desktop installer
must maintain a bounded ZIP extractor alongside the existing tar extractor.

Windows interruption and timeout code depends on Job Objects and thread resume
APIs. Spawn failures fail closed rather than launch an unowned process tree.
Tests that spawn children must use the same ownership abstraction when they
claim to cover production lifecycle behavior.

Windows scripts use PowerShell syntax. Existing repositories whose setup
commands are written only for POSIX shells need a platform-specific command or
a cross-platform command they invoke from PowerShell; Tidebreak does not
translate shell languages.

Direct process-environment capture does not observe changes made only by a
shell profile after Tidebreak starts. Users must restart Tidebreak after
changing machine or user environment variables. This is more predictable than
executing arbitrary profiles during every probe.

Revisit this decision if Windows stops shipping Windows PowerShell by default,
if a first-party process supervisor replaces local child ownership, or if code
workspaces move outside the Tidebreak data directory and require a broader path
authorization model.

## Validation

- Platform-pin tests require every shipped macOS, Linux, and Windows
  architecture to select one artifact and the correct executable layout.
- Archive tests exercise the real ZIP extractor and reject traversal,
  absolute, reserved-name, and link-like entries before installation.
- Harness tests require Windows managed npm and harness `.cmd` shims, exact
  markers, and managed Node precedence on `PATH` for probes and sessions.
- Environment tests require case-insensitive replacement of Windows keys and
  prove that profile scripts are not executed for capture.
- Script tests require PowerShell's non-profile, non-interactive invocation and
  exercise success, failure, timeout, and descendant cleanup.
- Worktree tests cover drive-letter, UNC, verbatim-prefix, and case-folded
  duplicate behavior.
- Native Windows CI runs managed-runtime, harness, and server code-mode suites.
  It must exercise repository registration, worktree creation, setup and quick
  actions, harness probe/launch, interruption, checkpointing, and restart
  recovery. Cross-compilation alone does not satisfy this validation.
