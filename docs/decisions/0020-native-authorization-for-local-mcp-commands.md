# 20. Native authorization for local MCP commands

- Status: Proposed
- Date: 2026-08-14
- Owners: desktop / MCP configuration
- Related: [`0004-self-host-deployment-plane-authorization.md`](0004-self-host-deployment-plane-authorization.md), [`../host-access.md`](../host-access.md)

## Context

The desktop renderer authenticates to the loopback API as the single local
owner. That is sufficient identity for conversations and ordinary deployment
configuration, but an enabled stdio MCP definition crosses a second boundary:
its `command` and arguments become a process running as the desktop user.

Treating the renderer bearer as sufficient for that transition means any
renderer compromise can silently turn into native process execution. CSP and
content isolation reduce the chance of such a compromise; they are not an
authorization boundary for the host process.

The desktop already has a separate client-executor credential that is withheld
from the renderer and attached only by native Rust code.

## Decision

An unmanaged web or self-host administrator may continue to configure MCP
servers through the authenticated deployment API. In the desktop profile, an
enabled `command` transport additionally requires the native client-executor
surface.

The renderer sends the candidate configuration to a Tauri command. When the
candidate contains enabled local commands, the native host displays an
OS-native warning naming the server and executable. Only an affirmative native
answer causes the host to forward the configuration with the client-executor
credential. The ordinary renderer API refuses enabled command definitions with
the stable `native_confirmation_required` error.

Disabled command definitions may be edited without confirmation because they
cannot start a process. Enabling them later crosses the native confirmation.
Remote HTTP and gateway transports remain on their existing authorization
paths; this record does not turn every settings write into a native prompt.

## Alternatives Considered

- **Trust the renderer bearer because desktop is single-user.** Rejected: it
  conflates user identity with host-process authority and removes a useful
  containment boundary after renderer compromise.
- **Disable stdio MCP servers.** Rejected: local MCP is a supported developer
  workflow, and a native confirmation preserves it without silent execution.
- **Return a reusable approval token to the renderer.** Rejected: renderer code
  could retain or replay it. The native host performs the privileged request
  itself and never reveals the client-executor credential.
- **Prompt for every MCP edit.** Rejected: URL-only and disabled definitions do
  not start host code. Prompting for them trains users to approve warnings that
  carry no matching risk.

## Consequences

Desktop saves use a Tauri command rather than a direct `PUT /mcp/servers`.
Headless and self-host clients retain the existing administrator API. Native
dialog availability becomes necessary to enable local commands in the desktop
app, and tests must cover both the renderer refusal and native acceptance.

Revisit this decision if the renderer is moved into a process sandbox that can
hold a narrowly scoped, non-replayable host capability, or if local MCP process
ownership moves to a separately authenticated broker.

## Validation

- A desktop bearer alone cannot save an enabled command server.
- The same candidate on the client-executor route passes the native-authority
  check.
- Disabled command and remote definitions retain their existing behavior.
- The Tauri command shows a native warning before forwarding an enabled command
  and never serializes the executor credential to the renderer.
