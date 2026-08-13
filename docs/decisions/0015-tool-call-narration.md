# 15. Model-authored tool narration is display-only

- Status: Accepted
- Date: 2026-08-13
- Owners: desktop UI / tool approvals
- Related: `ToolActionPreview` in `crates/tidebreak-core/src/preview.rs`,
  `GrantScope` in `crates/tidebreak-core/src/approval.rs`, the auto-approval
  judge in `crates/tidebreak-server/src/approval_judge.rs`

## Context

A tool-result card leads with the literal action. An `exec` card's collapsed
headline is the argument vector in monospace — `cat
.tidebreak/skills/word-documents/SKILL.md` — and expanding the card shows the
same string again above the output. So the one line a reader sees by default is
unreadable to anyone not reading shell, and the expanded state repeats itself.

Other assistants show a sentence there instead, and ask the model to write it:
the tool's own arguments carry a short description of what the call is doing,
rendered while it runs. Nothing in Tidebreak's preview pipeline carries such a
line today — `ToolActionPreview` is a closed, clamped projection of *arguments*,
and every consumer of it (result card, approval card, activity rail,
auto-approval judge, grant matching) reads the same value.

That last fact is the whole difficulty. Model-authored prose is untrusted input:
it is written by the same call it describes, and a call that could describe
itself to the consent surface could describe itself favourably. `rm -rf ~`
narrated as "Cleaning temporary caches" is the failure mode, and it is available
to a prompt-injected model as readily as to a mistaken one.

## Decision

Preview-carrying tools take an additional `summary` argument — one short
present-tense sentence for the person watching — and Tidebreak treats it as
**display-only**. It is carried on `ToolActionPreview` as
`Option<String>`, clamped to 200 characters through the same path as every other
preview field, and governed by three rules:

1. **It never appears on the approval card.** Consent is given to a command, a
   URL, a path — not to a sentence about one. The approval surface renders only
   `toolPreviewPresentation().detail`, which is built field by field from the
   literal action and never includes the summary.
2. **It never reaches the auto-approval judge.** `action_description()` composes
   judge input from named fields, not by serializing the preview, so a call
   cannot argue for its own approval in its own arguments.
3. **It is never part of a grant's identity.** `GrantScope::ExactAction` is
   minted and compared against `action.without_summary()`. Two runs of the same
   command narrated differently are the same action, and a standing grant covers
   both.

Included: the result card's collapsed headline, and the activity rail row for
non-`exec` calls. Excluded, deliberately: the approval card, the judge, grant
identity, `describes_exactly()` (narration is not part of the action, so a
narrated call remains exactly describable and stays grantable), and the
background-run activity timeline, which keeps argv this round.

The argument is **required in each tool's JSON schema** so models reliably write
one, and **tolerated in code** (`Option<String>`, absent deserializes as `None`)
so a call that omits it still runs and falls back to today's literal headline.
Note that a schemars-derived field carrying `#[serde(default)]` is dropped from
`required`, so the tolerance comes from `Option` alone.

`delegate_agent` takes no summary: its `task` field already is the prose.

## Alternatives Considered

- **Derive the line server-side from argv** (`cat X` → "Reading X"): needs a
  growing table of command-specific rules, is wrong for anything it has not been
  taught, and cannot describe intent — the model knows *why* it is running the
  command and no argv inspection recovers that. Rejected.
- **Show the summary on the approval card too**, as a subtitle above the
  command: this is precisely the social-engineering surface above. Even shown
  *beside* the literal command it shifts what a hurried reader actually reads.
  Rejected.
- **Make the summary part of the action identity** (no `without_summary()`): the
  simpler code, but standing `ExactAction` grants would silently stop matching
  whenever the model narrated the same command differently, re-prompting users
  for something they had already approved and teaching them to click through.
  Rejected.
- **Do nothing** and keep argv as the headline: honest and cheap, but the
  collapsed card — the only line most readers ever see — stays unreadable, and
  the expanded card keeps duplicating it. Rejected.

## Consequences

Every new preview-carrying tool now has a schema obligation (add `summary`) and
a review obligation (its narration must not leak into consent or identity
decisions). The renderer gains two headline paths — prose and literal — and the
literal one must stay reachable, which is why the argv keeps the expanded pane.
Tool-argument token cost rises slightly on every call.

Revisit if narration quality turns out worse than argv in practice (models
writing vague or misleading lines often enough that readers want the command
back by default), if a supported provider proves unable to fill a required
field reliably, or if a future consent surface genuinely needs prose — which
would demand a *server*-derived line, not this one.

## Validation

- A grant test: two calls with identical argv and different summaries are
  covered by one `ExactAction` grant. A wrong implementation that keeps the
  summary in identity fails here.
- A judge test: the composed judge input for a narrated `exec` call contains
  none of the narration text. This is the enforcement of rule 2 — the judge
  builder ignores unknown fields today by construction, and the test is what
  keeps it that way when someone later reaches for a serialize-the-whole-preview
  shortcut.
- A renderer test: `toolPreviewPresentation().detail` never carries the summary,
  so the approval card cannot show it. A plausible wrong implementation that
  adds narration to the shared presentation helper still renders a correct
  *result* card and would pass every card-level test — this is the case that
  catches it.
- A bounds test: an over-long summary is clamped by `build()`.
