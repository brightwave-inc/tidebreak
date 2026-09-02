# 81. Uneff me works without a Tidebreak checkout

- Status: Accepted
- Date: 2026-09-02
- Owners: code mode
- Related: [`0078-uneff-me-targets-the-tidebreak-product-repo.md`](0078-uneff-me-targets-the-tidebreak-product-repo.md),
  [`crates/tidebreak-desktop/ui/src/code/uneffMe.ts`](../../crates/tidebreak-desktop/ui/src/code/uneffMe.ts),
  [`crates/tidebreak-desktop/ui/src/code/startWorkspaceSession.ts`](../../crates/tidebreak-desktop/ui/src/code/startWorkspaceSession.ts),
  [`crates/tidebreak-desktop/ui/src/PastedText.ts`](../../crates/tidebreak-desktop/ui/src/PastedText.ts)
- Supersedes: 0078

## Context

Record 78 froze Uneff me as a dogfood path: the command was hidden until a
Code repo named `tidebreak` was connected, and it refused to start without
one. That made it invisible to the people it was for. A user who hit a
product bug reported that they could not find the button; they had no
Tidebreak checkout, so the menu never offered it.

Three more things fell short of the intent. The command handed the first
prompt to the new workspace's composer instead of sending it, so the reader
landed on an empty conversation with a hundred kilobytes of JSON in the box
and had to press send. Nothing on screen said what was happening between the
click and the new workspace. And the debug report went into the first turn
as bare text, so the transcript printed all of it, while a long clipboard
paste in the same composer is folded behind a chip.

Meanwhile workspace creation grew a handoff (`#2935`): an optimistic card in
the rail while the worktree is created, then a page-level step list until the
first session exists and the first message is sent. Uneff me should look like
that, not like a second design.

An earlier draft of this record cloned Tidebreak for the reader when no
checkout was connected. That was dropped before it merged: an issue needs no
source tree, a large clone spent on a question the agent has not asked yet
is a poor trade, and a product that clones repositories onto people's disks
because they asked for help reads as farming checkouts.

## Decision

Uneff me is offered on every workspace that has a session. It no longer
depends on which repos are connected, and it never clones anything.

When a connected repo is the Tidebreak product checkout by record 78's rule
(display name `tidebreak` or `brightwave-inc/tidebreak`, or folder basename
`tidebreak`, never a worktree path), the fix workspace is created there.
When none is, the session starts as a new agent in the workspace the reader
is already in, shown as the selected agent once it exists. The prompt says
which case it is.

The first turn is posted for the reader. The prompt tells the agent to ask
what went wrong and whether the user wants an issue or a pull request before
it investigates or changes anything. It names `gh issue create` against the
product repo. For a pull request from a Tidebreak checkout it says to open
the fix against `main` and to fork when the user cannot push. For a pull
request from the user's own workspace it says to ask where Tidebreak may be
cloned and to clone only after the user says yes, and to mention that adding
the Tidebreak repository to Code makes the next Uneff me start there. Because
the repository is public, the prompt also tells the agent never to paste the
whole report and to show the user any issue text before filing it. The
workspace title stays `Uneff: …`.

The debug report travels in the first turn as pasted text: the same
`<pasted_text>` wrapping the composer gives a long clipboard paste. A sent
message is split back into prose and paste blocks when the transcript draws
it, and each block renders as the folded chip the reader saw in the composer,
opening to the text with a copy control. This applies to every user message
in chat and code, not only to Uneff me.

The flow reuses the creation handoff rather than adding a second loading
state. The startup record gained a `preparing` phase with preparation steps
ahead of "Workspace ready"; the source workspace carries it while the report
is collected. With a checkout, the record moves to the new workspace once it
exists, the rail shows the optimistic creating card, and the new workspace
runs the shared first-session start — the same function the new-workspace
dialog now calls — through engine start and first message. Without one, the
same record stays on the source workspace through those steps.

A failed turn and an error-level engine notice in the transcript carry a
"File an issue" action that runs Uneff me on the session, so the way out of a
failure sits on the failure. Warnings and asides do not carry it.

The session's engine follows the source session's engine when it can start,
else the last create's, else any engine that can. The permission posture is
the last create's when the engine honors it, else the engine's create
default, clamped by the managed policy ceiling. If no engine can start, the
command reports that and does nothing.

## Alternatives Considered

### Keep the checkout gate and add a hint to connect Tidebreak

Cheapest, but it leaves the button hidden for the people reporting bugs, and
a hint to clone a repo by hand before asking for help is the errand the
command exists to remove.

### Clone Tidebreak for the reader when no checkout is connected

Makes every Uneff me PR-capable from the first click. Rejected, as above: it
spends a large clone before the agent has asked whether a PR is even wanted,
it writes a checkout onto the reader's disk without asking, and it reads as
farming clones. The agent can ask and clone when a PR is actually wanted.

### A separate loading surface for Uneff me

A toast or a dedicated dialog would have been quicker to write than
generalizing the startup record. It was rejected because the creation
handoff already answers "what is happening while my session appears", and a
second answer is what the design-system notes in `CLAUDE.md` forbid.

### Print the debug report in the transcript

Leaving the report as bare text is what shipped in `#2762`. It made the first
turn unreadable and made a code-mode user turn look unlike a chat turn with
the same paste. Folding it reuses a shape the reader already knows.

## Consequences

A reader with no Tidebreak checkout gets an issue path with nothing installed
or cloned, and a PR path that starts with the agent asking permission. The
rail card for their workspace shows the Uneff agent as the workspace's
remembered session until the page reloads, the same as any second agent
started from the page.

The debug report travels to a public repository only through the agent, which
is told to redact and to show issue text first. The report itself stays in
the session's first turn, folded.

Record 78's identification rule survives unchanged; only its refusal is gone.
The `canUneff` flag left the command menus, so nothing hides the item.

Revisit this if Code repo snapshots grow a remote identity that makes the
name heuristic unnecessary, if the first prompt should carry a redacted
report rather than the full one, or if "File an issue" should reach chat
errors outside code mode, which have no session debug report today.

## Validation

`uneffMe.test.ts` covers: the prompt asks before it acts and names both the
issue and the pull request path; the report is folded as pasted text and the
prose around it is not; the no-checkout prompt says to ask before cloning and
never claims a fresh workspace; a connected checkout gets a workspace and no
checkout gets none; engine choice follows source session, then last create,
then any; a plan ceiling yields plan rather than a refusal.
`workspaceActions.test.ts` pins that Uneff me appears next to Copy debug JSON
whenever there is a session and never without one. `PastedText.test.ts` pins
the split. `CodeTranscript.dom.test.tsx` pins the action on a failed turn and
an error notice, its absence on a warning and without a handler, and the
folded paste. The page test pins the handoff on the source workspace while
the report is collected, the first turn submitted rather than left in the
composer, and the no-checkout path creating a session here and nothing else.
The `UneffMePreparing`, `FailedTurnFilesIssue`, `EngineErrorFilesIssue`, and
`PastedTextFolded` stories show each surface. A plausible wrong
implementation that still gated on the checkout would fail the menu test; one
that cloned would fail the page test's clone assertion.
