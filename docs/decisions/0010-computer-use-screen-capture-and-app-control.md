# 10. Computer use: screen capture and app control

- Status: Proposed
- Date: 2026-08-12
- Owners: desktop / agent core
- Related: [`docs/deferred.md`](../deferred.md) (browser surface), permission
  modes and approval classes in `openwave-core/src/approval.rs`, host
  capability grants in `openwave-host-broker/src/capability.rs`

## Context

OpenWave can read approved folders, run code in sandboxes, and call configured
tools, but it cannot see the user's screen or operate the applications in
front of them. That keeps a whole class of "help me with what I'm looking at"
work out of reach: reading a window the user is describing, working in an app
the agent cannot reach through files, or checking that something it produced
actually renders.

Facts that shape the design:

- The desktop app already routes host-native tool calls through a durable
  claim/heartbeat/resolve loop (`openwave-server/src/routes/client_execution.rs`
  ↔ `openwave-desktop/src/client_execution/`), so a tool that must run where
  the display is has an established execution path.
- Tool outputs already carry images end to end: `ToolOutput::with_images`,
  blob publication in `agent/dispatch.rs`, and model-facing
  `ContentBlock::Image` emission gated on the model's `image_input` flag in
  `agent/transcript.rs`. Screenshots need no new wire types.
- The `openwave-host-broker` sidecar is the deny-by-default authority for host
  capabilities (today: folder access). It is the natural home for screen and
  app-control grants, keeping policy out of the webview and away from the
  model-reachable surface.
- macOS mediates both halves behind TCC: Screen Recording for capture,
  Accessibility for reading UI trees and synthesizing input. TCC binds grants
  to a binary's code signature and path, so the process holding these
  permissions must live at a stable, signed location; moving or re-signing it
  resets the user's grants.
- Approval machinery exists and is calibrated elsewhere: `ApprovalClass`,
  `ToolApprovalKind`, standing grants, per-chat permission modes
  (Plan/Ask/Auto/Allow).

The risk landscape is known. Screen contents are untrusted input (prompt
injection via whatever is on screen). Input synthesis can trigger
irreversible, externally visible actions — sending, buying, deleting. And
over-prompting is its own failure mode: a permission system that interrupts
for every action trains users to click through, which is worse than asking
less and meaning it. OpenWave's standing posture is that safety for granted
capabilities comes from visibility, interruption, and audit, not from a modal
per action.

## Decision

Build computer use as a first-class, consent-gated capability.

**Scope.** macOS only, desktop app only, foreground chat only. The tools are
client-executed; sandboxed and background agents can never hold them — they
run where there is no display. Enablement requires the master setting on, a
macOS desktop client attached, and a selected model that accepts image input.
When the model cannot accept images, capture tools refuse with a typed error
rather than degrading silently.

**Architecture.** Three layers with a strict trust gradient:

1. A small signed Swift helper, bundled at a stable path and spawned per
   operation over JSON stdio. It talks to ScreenCaptureKit (capture),
   the Accessibility API (UI trees, element resolution), and CGEvent (input
   synthesis). It is a dumb executor: no policy, no persistence, bounded
   output, hard timeout.
2. `openwave-host-broker` holds all policy: new capabilities `CaptureScreen`,
   `ReadAppContent`, `ControlApp` scoped per app bundle id (plus a
   whole-screen capture scope), the app blocklist, the consequential-action
   classifier, and the append-only audit of every operation. Control intent is
   recorded durably before input is synthesized; if the audit cannot be made
   durable the action refuses. Granting `ControlApp` implies the two read
   capabilities.
3. The agent surface: named client-executed tools registered in
   `openwave-core`, claimed and fulfilled by a desktop module that calls the
   broker, with screenshots entering the transcript through the existing
   image pipeline.

**Tool surface.** Narrow, named tools rather than one generic controller:
`computer_list_windows`, `computer_capture_screen`, `computer_read_app_content`,
`computer_click`, `computer_type_text`, `computer_key_press`,
`computer_scroll`, `computer_focus_window`, `computer_return_to_openwave`,
`computer_wait`. Targeting is accessibility-first: elements are addressed by
tree path plus a content fingerprint that detects drift between look and act,
and screenshots carry numbered visual marks over interactive elements that
resolve back to accessibility elements. Raw coordinates are a documented last
resort for apps with no usable accessibility surface. Element resolution is
re-checked at act time; a changed element refuses as stale rather than
clicking whatever is now under the pointer.

**Consent and approval — one surface, calibrated.** The layers a control
action passes are:

1. OS TCC grants (Screen Recording + Accessibility), requested together on
   first use with a status checklist in settings.
2. A per-app grant, asked once per app through an OS-native dialog owned by the
   Tauri host ("Allow Tidebreak to control Mail — once / always for this chat /
   always"). The renderer may show that computer use is waiting, but it never
   receives the pending call identifier and exposes no command that can resolve
   the decision. The native answer is written into the broker's grant store;
   the broker enforces. Grants are listed and revocable in settings.
   Whole-screen capture gets the same treatment as one standing grant.
   **Once is not a standing grant that the
   desktop later revokes.** It is a broker-native `single_use` grant:
   session-only (never written to the durable grant table), hidden from the
   grants listing, and consumed when the operation it authorized reaches a
   terminal result. A confirmation hold is not terminal — the same one-shot
   covers the confirm. A crash or a failed revoke therefore cannot promote
   once into a durable per-conversation grant. Chat and Always remain
   persisted standing grants.
3. An act-time OS-native confirmation only for consequential actions: before
   clicking, the broker re-reads the target element and classifies it; labels that
   commit an external effect (send, pay, buy, delete, submit, sign,
   transfer, post) or credential fields force a single non-activating
   confirmation, honored only if the label still matches at act time. The
   renderer receives neither the confirmation identity nor an API that can
   redeem it. The
   classifier's word list is deliberately short and **English-only** —
   navigation-shaped words (cancel, back, close, decline) do not trip it.
   Localized commit verbs ("Envoyer", "Senden") are an accepted miss of the
   same calibration: a translation dictionary would over-ask in those
   locales the way a longer English list over-asks here. An activatable
   control (button / menu item / link) with no accessible label cannot be
   classified as navigation and is treated as consequential. Icon-only
   chrome that is not an activatable role stays benign.

Within a granted app, individual clicks, keystrokes, and scrolls do not
generate approval cards — in any permission mode. Plan mode refuses control
tools outright and allows reads. Reads (window list, capture, accessibility
tree) never card once their grant exists. There is no LLM auto-approval judge
in the loop for control actions.

**Always-on safety, independent of consent state:** a persistent
"OpenWave is controlling {app}" indicator with a Stop button that halts
before the next broker round-trip and tells the agent not to retry; an
auto-yield that aborts any control operation while a security surface
(login window, authentication prompt) is frontmost, as a non-retryable
error, carried as a structured `ErrorCode::Yielded` (never a `Denied`
that the desktop could mistake for a grant miss); and a hard app
blocklist — Tidebreak itself, terminals, IDEs and editors with an
integrated shell, command launchers (Alfred, Raycast), System Settings,
keychain and security agents — enforced in the broker and mirrored
defensively in the helper. A bundle-id list cannot enumerate every app
that embeds a shell; the listed class is the known high-traffic set, not
a proof of completeness.

**App knowledge lives in plugins, not code.** No per-app drivers. App-specific
guidance (how an app's UI is laid out, preferred flows, what to avoid) ships
as instruction-only plugin skills the user can install and invoke. The
starter set targets Notes and Preview (well-behaved accessibility trees,
low-stakes content) with Mail as the proving ground for the consequential
gate. Browsers are deliberately not in the starter set: the everyday browser
remains the separate deferred surface described in `docs/deferred.md`, and
computer use does not become a back door to it.

**Deliberately excluded from this decision:** Windows and Linux support,
scripted per-app automation (AppleScript-style typed intents), controlling
the user's browser as a substitute for a real browser surface, screenshot
content redaction, and any auto-approval of control actions.

## Alternatives considered

- **A single generic `computer` tool driven by screenshot coordinates** (the
  shape some provider-native computer-use tools take). Rejected: coordinate
  loops are slower (a capture per action), fragile under retina scaling and
  window movement, and unauditable — a click at (412, 305) cannot be
  classified or confirmed meaningfully. Accessibility-first targeting gives
  every action a named target that policy can reason about.
- **Per-app scripted drivers** (AppleScript/automation APIs per integration).
  Rejected for v1: each driver is a bespoke, breakable contract, and the
  accessibility channel already generalizes. Skills carry the per-app
  knowledge instead. Revisit for apps where accessibility is truly unusable.
- **Approval card per control action.** Rejected: it makes the capability
  unusable for any multi-step task and trains click-through. The per-app
  grant plus the consequential gate covers the same risk with two prompts
  instead of dozens.
- **An LLM judge auto-approving control actions in Auto mode.** Deferred: the
  judge adds a second policy brain whose failure modes are hard to audit, and
  the calibrated consent layers should make it unnecessary. Revisit if per-app
  grants prove too coarse in practice.
- **Renderer-hosted consent and confirmation cards.** Rejected: a renderer
  compromise could enumerate pending call identifiers and resolve its own
  approval. The native host owns the only asking surface. There is still one
  grant system and one prompt per decision; moving it outside the renderer does
  not add a second policy brain.
- **Do nothing / wait for the managed browser surface.** Rejected: most of the
  value (seeing the screen, operating native apps) is orthogonal to the
  browser plan and blocked on nothing.
- **Persist a once grant and best-effort revoke it after the op.** Rejected:
  a crash or a failed revoke between write and revoke leaves a standing
  per-conversation grant the user approved as one-time-only. A broker-native
  session-only one-shot has no revoke to lose.
- **Match the security-surface yield by comparing the broker's English error
  message.** Rejected: any wording edit silently converts a hard-stop yield
  into a consent prompt. The transport carries `ErrorCode::Yielded`.

## Consequences

- A new signed native binary enters the bundle with strict path/signature
  stability requirements; CI and release tooling must build and sign it, and
  its location becomes a compatibility contract with users' TCC grants.
- The broker grows from a file-capability authority into the general host
  capability authority. Its grant vocabulary becomes persisted state covered
  by the pre-1.0 schema mutability rules.
- The approval system gains a `ToolApprovalKind` for app control whose
  standing-grant semantics differ from existing kinds (grantable per app, not
  per tool); the grants UI must render it comprehensibly.
- Prompt-injection exposure widens: screen contents flow into the transcript
  as model input. The system prompt must state that screen content is data,
  not instructions, and the consequential gate is the enforcement backstop.
- Background agents are permanently second-class for this capability; any
  future "agent works while you're away" story that wants screen control will
  need its own decision.
- Revisit this decision if: TCC attribution for a broker-spawned helper
  proves unworkable on real hardware (the helper may need to become an app
  extension or move process ownership); per-app grants prove too coarse or
  too naggy in field use; a Windows port begins; or the managed browser
  surface lands and browser-adjacent use of computer use needs re-drawing.

## Validation

- End-to-end: a real turn against a scratch app — capture, read tree, click a
  named element, type, and read the journal to confirm audit entries precede
  synthesis and the screenshot blob round-trips into a model image block.
- The consequential classifier is pure and test-covered: commit-shaped labels
  and credential fields trip it; navigation labels do not; an implementation
  that classified nothing would fail these, and one that classified
  everything fails the navigation cases.
- Blocklist and auto-yield tests: control ops against blocked bundle ids and
  while a simulated security surface is frontmost must refuse with
  non-retryable errors — a plausible wrong implementation retries on these.
- Stale-element test: mutate the target between look and act; the op must
  refuse, not click.
- Image-input gating: with a text-only model selected, capture tools refuse
  with the typed error; nothing silently drops the image.
- Grant-store test: a card decision at each scope produces exactly one broker
  grant, revocation removes it, and no control op proceeds without one. The
  renderer snapshot contains no pending call identifiers, and command-parity
  coverage proves no renderer command can resolve consent or confirmation.
- Once-grant test: a `single_use` grant authorizes exactly one terminal
  operation (including a held confirm), does not appear in the grants
  listing, and is absent after a broker reload. A standing grant at the
  same tuple replaces a leftover one-shot.
