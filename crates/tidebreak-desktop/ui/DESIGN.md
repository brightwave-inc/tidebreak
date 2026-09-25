# Tidebreak desktop design system

The tokens live in `src/styles.css` (`:root`, `.dark`, `@theme inline`); this
file says what they mean and the rules that keep the app coherent. The rules
that can be checked mechanically are checked: `src/stylesContract.test.ts`
fails the build on arbitrary font sizes, raw palette classes, and text dimmed
with an alpha, and `src/colorContrast.test.ts` fails it on any color pair
below WCAG contrast in either theme, so the vocabulary below is the whole
vocabulary.

Storybook shows the system: `Foundations/Palette`, `Foundations/Typography`,
`Foundations/Surfaces`, and `Foundations/Controls and status`. Run
`scripts/storybook.sh` from the repository root.

## Identity

Four decisions carry the app's character. Everything else follows from them.

1. The neutrals are cool. Every gray carries a trace of blue (oklch hue 240)
   at chroma too small to read as color on any one surface. It is the
   temperature of the whole app, in both themes. Do not add a pure gray
   (chroma 0) or a warm gray; match the ramp.
2. One accent is ours: `live` teal. It means an agent is doing something
   right now, and it appears nowhere else. Status hues (green, amber, red,
   blue, purple) mean outcomes and states; teal means "working, this second."
   Rationing is what keeps it a signal.
3. Two voices. Geist is how the product speaks; Geist Mono is how the machine
   speaks. Branch names, SHAs, file paths, diffs, terminal output, model
   identifiers, and keyboard shortcuts render in `font-mono`. Prose,
   labels, and controls render in the sans.
4. Depth is drawn with hairlines, not shadows. Borders and surface steps
   separate regions; shadows are reserved for things that genuinely float.

## Color

Semantic tokens only. Components never use Tailwind's raw palette
(`text-emerald-600`, `bg-sky-500`); the contract test allows exactly three
exceptions, all of them document-viewer conventions: syntax colors in
`json-viewer.tsx` and `xml-viewer.tsx`, and the highlighter yellow in
`citationMark.ts`.

Status is a six-tone vocabulary, and every tone ships the same five-member
quad (`--x`, `--x-foreground`, `--x-foreground-muted`, `--x-background`,
`--x-border`) in both themes:

| Tone | Means |
| --- | --- |
| `success` | finished well; checks green |
| `warning` | needs a look; stalled, fenced |
| `critical` | failed, or needs you now |
| `info` | in flight but waiting on something external |
| `merged` | settled outcome distinct from success (GitHub's purple) |
| `live` | an agent is doing work right now |

Each member of the quad has one job, and `src/colorContrast.test.ts` holds
every pair to WCAG contrast in both themes:

| Rung | Job | Floor |
| --- | --- | --- |
| `--x` | the mark: a glyph, dot, or fill that carries the state alone | 3:1 on every surface and row |
| `--x-foreground` | the text rung: labels, and icons sitting beside them | 4.5:1 on every surface, row, and its own tint |
| `--x-foreground-muted` | the text rung at 90%, for chips | 4.5:1 on the same grounds |
| `--x-background` | a quiet tint just off the canvas | chroma 0.06 or less |
| `--x-border` | the tone's outline | — |

The dark quads are derived, not the light quads swapped. Dark text sits at
L 0.80 with enough chroma to keep its hue, the tint sits at L 0.28 just above
the canvas, and the outline is the mark at 60%. A swapped quad turned dark
text near-white and dark tints into saturated slabs.

The critical mark doubles as error ink: it clears 4.5:1 as text on every
surface and row, so `text-critical` can carry an error message on the page.
`destructive` is shadcn's name for the same red. Fills and borders may use
either name, but text uses `text-critical`; `stylesContract.test.ts` rejects
`text-destructive`. No other mark reads as text; put text in its
`-foreground` rung.

Dim text with a token, never an alpha. `text-muted-foreground` at full
strength is the secondary ink, and `stylesContract.test.ts` rejects a text
color below 90% alpha. Icons are marks, not text, and are exempt. An
`opacity-*` utility on text that says something is the same mistake, even
though no test catches it.

In code mode, never pick rungs by hand: `src/code/statusTone.ts` is the one
place a state becomes a color, and its maps (`STATUS_TEXT`, `STATUS_MARK`,
`STATUS_DOT`, `STATUS_CHIP`) pick the rung for the surface being painted.
Elsewhere, the `Badge` variants carry the same tones.

Pull requests are their own sub-vocabulary on top of these tones:
 `src/code/prState.ts` is the one place a pull request's lifecycle, gate,
 and chips are decided, and it paints with the same tones — green open,
 gray draft, purple merged, red closed, the info tone for a merge queue or
 armed auto-merge. A surface that renders a pull request from raw host
 fields is a bug.

Identity is not status. File types, repository swatches, and engine badges
use the `--icon-*` family (`icon-blue` through `icon-green`), which lifts
from the 600 step in light to the 400 step in dark. A `.json` file must
never read as a warning.

If you reach for a `dark:` override on a color, first check whether a token
already models both themes; most do.

Code in a diff is document content, like the JSON viewer's colors, so it has
its own two families. The `--diff-*` tokens are the diff's grounds: an added
or removed row takes its status tint at half strength, the words that
changed inside it take a stronger step of the same hue, and a row picked for
a comment takes the info tint over whatever it wears. The `--syntax-*`
tokens are six roles of code ink (keyword, string, comment, number, title,
attr), used only inside code. `colorContrast.test.ts` holds every role, and
the plain foreground, to 4.5:1 on every diff ground in both themes, so a
role can never fight a tint. Never use a syntax color for status or chrome.

## Type

The root is 14px and the scale is pinned in px, because the app is a dense
desktop tool and rem-derived sizes landed between the rungs people actually
wanted. Zoom scales the webview, so px sizes zoom correctly.

| Token | Size / line | Job |
| --- | --- | --- |
| `text-2xs` | 10 / 14 | micro labels, counters, avatar initials |
| `text-xs` | 11 / 15 | dense metadata, timestamps, eyebrows |
| `text-sm` | 12.5 / 18 | the working size for chrome: rows, menus, buttons |
| `text-md` | 13.5 / 20 | content: transcript bodies, card titles, approval prose |
| `text-base` | 14 / 21 | body text |
| `text-lg`–`text-3xl` | 16–24 | headings, welcome screens |

Arbitrary sizes (`text-[13px]`) are a contract-test failure. If a real new
rung emerges, add it to the scale in `styles.css` and to this table, and say
what its job is.

Weights: 400 for text, 500 for emphasis and control labels, 600 for titles.
Nothing heavier; hierarchy comes from size, weight stops at semibold.
Eyebrow labels are `text-2xs`/`text-xs` uppercase with `tracking-wide`, and
usually mono when they label machine output.

## Surfaces and elevation

Two surface tokens do the structural work. `page-background` is the app
canvas; `background` is the reading surface (panes, cards, composer) and
sits one step off the canvas in both themes — lighter in light mode, darker
in dark mode. `muted` recesses a region within a surface; `popover` is the
overlay surface.

Regions separate on `border-border` hairlines; `border-subtle` divides
within a card. There are exactly three shadows — `shadow-sm`, `shadow`,
`shadow-lg` (Tailwind's other steps alias onto them deliberately) — and
they belong to things that float: menus, dialogs, drag previews, the
computer-use HUD. Cards on a surface take a border, not a shadow.

## Shape and controls

`--radius` is 0.5rem, 7px at the 14px root; the derived steps `sm`, `md`,
`lg`, and `xl` (3, 5, 7, and 11px) and full pills are the whole radius
vocabulary. Status chips and badges are pills; cards are
`rounded-xl` at most.

Controls sit on the two pinned heights, `h-control` (32px) and
`h-control-sm` (28px). The primary button is neutral (near-black in light,
near-white in dark); `destructive` is the only chromatic button: critical ink
on the critical tint. Every button, destructive included, focuses with the
`ring` token, which is darker than stock so it survives hovered rows.

## Don't

- Don't add a new hue, a chromatic button fill, or a second accent.
- Don't use `live` for anything that is not running right now.
- Don't put drop shadows on resting cards.
- Don't use raw palette classes or arbitrary font sizes; the contract test
  fails both.
- Don't dim text with an alpha or an opacity; use a text token.
- Don't exceed weight 600.
- Don't hand-pick status rungs in code mode; go through `statusTone.ts`.
- Don't paint a feature nobody has set up as critical.

## Extending the system

A new status tone is a five-member quad in both themes in `styles.css`, an
entry in each `@source inline(...)` line, a row in the `statusTone.ts` maps,
a `Badge` variant, a swatch row in `Foundations/Palette`, and an entry in the
tone list of `colorContrast.test.ts`. A new scale
rung is a `--text-*` pair in `styles.css` plus its row here and in
`Foundations/Typography`. If a rule in this file and the code disagree, fix
one of them in the same change.

## Patterns

A component library gives you parts. These rules carry the decisions that
make a screen feel like Tidebreak. When you build a surface, classify it
first, then reach for the pattern that owns that kind of work.

### Surface classification

Every region is one of four surfaces. The surface decides the density,
the interaction model, and how much chrome is allowed.

| Surface | Work | Interaction model | Chrome |
| --- | --- | --- | --- |
| **Orienting** | "What needs my attention right now?" | Scanning, launching | Spacious, expressive |
| **Index** | Locating or comparing many records | Sorting, filtering, opening | Dense rows, compact controls |
| **Bulk edit** | Mass-entering or mass-editing values | Inline editing, keyboard flow | Compact, stable row rhythm |
| **Resource detail** | Reading and editing one record | Focused editing in a sheet | Single-column, bounded width |

A surface is not a route. A settings page can host an Index surface
(the rail) next to a Resource detail surface (the panel). A chat transcript
is an Orienting surface that embeds Approval cards (Resource detail
interactions).

### Settings pages

`SettingsPanel` owns the page shape: title, description, bounded column,
and the rhythm between sections. Do not set a different page width or
header layout. Do not use tabs or a card per setting.

`SettingsSection` groups related fields. Put identity or account details
first. Put destructive actions last, in a section named "Danger zone" or
equivalent.

`SettingsField` is a label and its control in one `label` element. The
control sits below the label and spans the full width, because selects,
text inputs, and editors read badly squeezed against the right edge.

Save each field when its value changes. Do not add Save or Cancel buttons.
Use a `Switch` for binary toggles and a `Select` for short lists. Use an
async validation pattern for text that must be checked, such as a workspace
URL.

`SettingsStatus` is the verdict a settings surface leads with. It is a
`Notice`, a neutral surface with the tone on its leading edge and icon, and it
speaks the status vocabulary:

| Tone | Means | Icon |
| --- | --- | --- |
| `ready` | set up and working | `CircleCheck`, success |
| `neutral` | off, or not set up yet | `CircleMinus`, muted |
| `warning` | needs a look: half working, nearly full, missing a key | `CircleAlert`, warning |
| `critical` | a real failure | `CircleAlert`, critical |

An optional feature nobody has set up is `neutral`, never red.
`SettingsError` is a critical `Notice`. When the failure is the panel's own
load, pass `onRetry` so the reader can run the load again.

### Cards and rows

`ToolCardShell` is the canonical expandable row for a tool call or agent
step. The collapsed state is boxless: a transcript should read as a
conversation with occasional notes about what ran, not as a stack of
nested panels. Keep the icon small (`size-3.5`), align it with the primary
text line, and keep the title on one line. Expand while work is still
happening, and expand a failed card so the error is one click away; a
settled, successful card collapses to a single row.

`ApprovalCard` is the canonical consent surface for command, file, network,
and tool-use grants. It leads with a short question, shows the exact action in
a muted preview block, and lists choices as numbered rows ordered narrowest
grant first. Question and plan continuations use their own answer protocols;
never render them as binary approval choices. Destructive or irreversible
actions open a dialog instead; do not use an approval card for a delete
confirmation.

`ChatStatusChip` is the canonical activity summary for a conversation.
It shows live work first, outputs otherwise, and collapses to a compact
pill when a side panel is using the canvas. Do not build a second activity
summary that duplicates outputs, folders, permissions, or agents.

### Diffs

`DiffView` draws every diff: the workspace against its base, one turn's
changes, and a pull request's files. Do not build a second renderer. It
carries one type scale (`text-md` code on 20px rows, `text-xs` line numbers),
both gutters, one marker column the reader cannot copy, syntax color, and
word emphasis, unified or side by side.

The line numbers are how a reader comments. Where the diff takes comments,
each number is a button in one roving tab stop: arrows move, Shift extends,
Enter opens the editor under the lines. A diff that takes no comments draws
plain numbers and makes the region itself focusable. Pending comments are
cards under their last line, bordered, never shadowed. A comment whose code
changed sits at the top of the file with its quote and a warning-toned
Outdated pill; it never moves onto whatever now sits at its old number. A
file that leaves the diff while it has comments stays first in the list,
under its path and "No longer in this diff", with those comments outdated.

### Live labels

While a status line is live — Thinking, an in-progress tool phase — the
label keeps muted ink and a highlight of `--foreground` sweeps across the
glyphs (`.live-label-shimmer`). Do not pulse the label's opacity, do not
cycle invented statuses, and do not paint the text with `live` teal. The
sweep stops under `prefers-reduced-motion`.

### Empty states and welcome surfaces

`Empty` is the canonical null-state container, and `EmptyMedia variant="icon"`
is how an empty index reads across the app: Inbox, Folders, Outputs, Plugins,
Apps, and the Code pages all use it. The variant renders the icon at `size-11`
in muted or identity ink (`text-muted-foreground`, `text-success`,
`text-icon-violet`, and friends); it draws no filled container behind the
icon. Give the empty state one useful next action when one exists.

Two first-run surfaces are their own compositions, not `Empty` variants:
`WelcomeState` (logomark plus starter prompt cards) and `CodeRepoEmptyState`
(the split layout with onboarding steps). `Modes/Null states` shows both. Do
not rebuild them from `Empty`.

In high-density operational surfaces (menus, tables, repeated rows), do not
place an icon in a circle, square, tint, or decorative container. Keep the
icon small, outline-weight, and aligned to the primary text line. Render
static attributes as plain text by default; reserve badge treatment for
status indicators.

### Icons

Lucide icons are the only icon set. Use them as lightweight recognition
markers, not as decorative advertising.

- Size the icon to match the surrounding text: `size-3.5` for dense rows,
  `size-4` for controls and labels, `size-7` only for low-density empty states.
- Keep the icon close to the label it identifies.
- Align the icon to the primary text line, not the vertical midpoint of a
  title and metadata stack.
- Use `text-muted-foreground` for icon ink in operational surfaces. Use the
  `--icon-*` family only for identity (file types, repository swatches,
  engine badges), never for status.
- Do not place an icon in a colored or gray container on high-density
  surfaces. `EmptyMedia variant="icon"` draws no container at all, and it is
  the only icon treatment allowed on an empty surface.
- Live agent, workspace, tool, and plan status uses the `Loader` comet variant
  in the `live` tone. Every comet shares one rotation phase, so a tool that
  starts later stays in step with the workspace mark already spinning.
  Indeterminate action progress uses `Spinner`; a busy refresh control swaps
  to that glyph. Do not put `animate-spin` on `RefreshCw` or `RotateCw`:
  Lucide's ink box is not the viewBox center, so the glyph orbits.

### Destructive actions

A destructive action goes through `useConfirm`, which opens the shared
`AlertDialog`: a title that asks the question, a description that names the
consequence, a safe Cancel default, and the `destructive` action variant on
the confirm button. `destructive` is the only chromatic button fill. Do not
add a second confirmation shape.

Typing to confirm is reserved for the one action that cannot be undone and
takes everything with it: Delete all data, which erases the profile and its
keychain items and quits. It uses the same dialog with `requireText`, and the
confirm button stays disabled until the phrase is typed. Do not ask for typing
anywhere else; deleting a conversation or a memory is confirmed with the
button alone.

### Notices and errors

`Notice` (`components/ui/notice.tsx`) is the one shape a notice or a failure
takes: a neutral surface with a hairline border and the `lg` radius (`md`
when compact), the tone on a straight 2px bar along the leading edge and on
the icon, the message in normal ink, and an action slot. The bar stops one
radius short of each end, so it never bends around a corner into a bracket,
whether the corner is the notice's own or the pane's a docked strip meets.
It speaks the status tones (`critical`, `warning`, `info`, `success`) plus
`neutral`. Do not fill a notice with a status color, and do
not draw a notice box by hand: `stylesContract.test.ts` rejects the retired
`notice-surface`, `.message-notice`, `.message-turn-failure`, and
`.settings-status` classes. `Components/Notice` in Storybook shows every tone,
the action slot, long text, and a narrow panel.

- Lead with a short title in sentence case ("Could not load your apps"), and
  let the body say what happened and what to do next. Do not apologize.
- A panel-level failure inside an Index or Resource detail surface is a
  critical notice whose action is `NoticeRetryButton`, wired to the panel's
  own reload or refetch, never to a page reload. While that reload runs,
  pass `pending` (or use `useRetry`) so the button waits instead of sending
  another request, unless the reload clears the failure as it starts. An
  empty index is not a failure: it keeps `EmptyMedia variant="icon"` and
  never becomes an error box, and a result with nothing in it is a plain
  message, not a red alert.
- Validation is not a failure. Why a value cannot be saved sits under its
  field in critical ink (`SettingsField`'s `error`, or `SettingsFieldError`)
  and never offers a retry. A load or a save that did not go through is a
  `SettingsError`, the settings notice.
- A state the reader cannot fix by retrying, such as an archived workspace,
  is a `neutral` notice with no Retry.
- Word every failure with `friendlyErrorMessage` from `lib/utils.ts`. It drops
  the class name and status code `String(err)` shows, words a request that
  never reached the server as "Tidebreak could not reach its server. Check
  that the app is running, then try again.", and keeps the server's own
  message, because that is usually the detail the reader can act on. Renderer
  copy replaces it only for the kinds whose text is never written for a
  reader (`store`, `serde`, `secret`); pass kind copy where your context knows
  more, such as a missing app. `errorMessagesContract.test.ts` rejects
  `String(err)`, `` `${err}` ``, `err.toString()`, `JSON.stringify(err)`, and
  concatenating a caught value, except on a line marked
  `raw-error-ok: <reason>`.
- `docked="top"` or `"bottom"` fits a notice to a pane edge as a strip.
  `density="compact"` sets it in the dense chrome size for transcripts,
  composers, and rows. Put machine output under the message in
  `NoticeDetail`.

In chat and code journals, notices span the full message column regardless of
text length or severity. Do not add a prose-width cap or size a notice to its
content. Compaction stays a plain text event in the same column.

A page that crashes keeps its rail: every route under a layout sets
`errorComponent: RoutePaneError`, which says so in the pane with Try again, Go
home, and Copy debug info. A crash in the shell or a layout takes the window
through `RouteCrashScreen`, the same screen the app-wide `ErrorBoundary`
draws. An address no route answers renders `RouteNotFound`: an `Empty` state
with a way home, inside the frame.
