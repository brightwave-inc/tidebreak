# 93. Computer use for coding harnesses

- Status: Accepted
- Date: 2026-09-08
- Delivery: #3245
- Supersedes the foreground-only, development-app blocklist, and screenshot-redaction limits in decisions 13 and 54.

## Decision

Code sessions receive computer use through a host-owned service. External harnesses use the bundled MCP bridge, with a CLI image-file fallback where necessary. The internal engine calls the same runtime. The existing browser channel remains compatible. Browser and native operations keep distinct schemas and permission scopes.

Screenshot access is part of an explicitly approved app or browser scope. Consent says that visible content reaches the selected model and provider. Automatic redaction is not a guarantee or a prerequisite for capture. Whole-display capture requires its own grant. Existing browser sharing must receive the disclosure before gaining screenshot access.

Native app access does not silently replace denied browser access. Native control of browser windows requires a broader app grant that explains its scope. System authentication and approval interfaces remain under human control. Testing Tidebreak itself must use an isolated development target without access to the controlling instance's approval interface.

Terminals, editors, IDEs, and command launchers use the same explicit app grants as other apps. Control consent for these apps states that the agent can run commands with the user's local account permissions, including outside a coding sandbox. Read and screenshot grants do not authorize control. A list of executable apps cannot enforce a sandbox boundary, so the known development-app list only selects the stronger disclosure.

The hard blocklist retains the `io.brightwave.tidebreak` app family and OS authentication, keychain, and settings surfaces. It does not block other products under `io.brightwave.*`. To test Tidebreak, build a separate target with a bundle id under `dev.tidebreak.*` and an isolated profile without access to the controlling instance's approval interface. A renamed bundle alone does not establish that isolation. Reserved Tidebreak app ids remain blocked even when a grant exists.

The host derives owner, workspace, and session identity. Each operation carries a request id. Completed results may be recovered; unknown outcomes require inspection before another action. An interrupt cancels pending input and releases ownership. Session termination revokes access. Restart invalidates transient targets and controllers.

Independent tabs may operate concurrently only when their adapter does not use desktop input. Any operation that uses mouse, keyboard, or foreground focus holds exclusive host input ownership. Human takeover ends that ownership before another queued action can begin.

The three adapters cover Tidebreak's in-app browser, native macOS apps, and Chrome. Chrome uses an approved local debugging connection; an extension can provide equivalent integration. The model cannot supply arbitrary debugger endpoints or use full debugger access to bypass narrower site grants. Developer diagnostics require explicit authority.

## Implementation contract

`computer_session::ComputerUseCall` and `ComputerUseResult` separate request identity, outcome, text, structured data, and image bytes. Native tool schemas and validation come from `computer_use_tool_specs` and `validate_computer_use_arguments`. Transport code does not duplicate tool schemas or implement grants.

The native runtime must reuse the host broker and native consent path. Browser work retains its existing origin, visibility, controller, and stale-target checks. Image adapters fit results within the supported transport budget without dumping base64 into model text.

## Qualification

- Every supported harness receives the intended tool set and actual image content.
- In-app browser, Chrome, and native app workflows cover launch, observation, click, type, hover, drag, scroll, waits, screenshots, and verification.
- Real coding tasks reproduce a problem, edit code, rebuild, and repeat the UI flow.
- Tests cover wrong-session calls, changed targets, Stop, takeover, reconnect, uncertain outcomes, and concurrent input.
- macOS native and packaged evidence is recorded separately from Linux or simulated tests.

The integration PR has no unresolved P1 or P2 findings. Reviewers may report no findings. Formatting, focused behavior tests, build checks, and real native acceptance determine completion.
