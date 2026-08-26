# Tidebreak desktop design system

The tokens live in `src/styles.css` (`:root`, `.dark`, `@theme inline`); this
file says what they mean and the rules that keep the app coherent. The rules
that can be checked mechanically are checked: `src/stylesContract.test.ts`
fails the build on arbitrary font sizes and raw palette classes, so the
vocabulary below is the whole vocabulary.

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

`--radius` is 8px; the derived steps (4, 6, 8, 12) and full pills are the
whole radius vocabulary. Status chips and badges are pills; cards are
`rounded-xl` at most.

Controls sit on the two pinned heights, `h-control` (32px) and
`h-control-sm` (28px). The primary button is neutral (near-black in light,
near-white in dark); `destructive` is the only chromatic button. Focus is
the `ring` token, darker than stock so it survives hovered rows.

## Don't

- Don't add a new hue, a chromatic button fill, or a second accent.
- Don't use `live` for anything that is not running right now.
- Don't put drop shadows on resting cards.
- Don't use raw palette classes or arbitrary font sizes; the contract test
  fails both.
- Don't exceed weight 600.
- Don't hand-pick status rungs in code mode; go through `statusTone.ts`.

## Extending the system

A new status tone is a five-member quad in both themes in `styles.css`, an
entry in each `@source inline(...)` line, a row in the `statusTone.ts` maps,
a `Badge` variant, and a swatch row in `Foundations/Palette`. A new scale
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
Use a `Switch` for binary toggles and a `Select` for short lists. Use
`ValidatedInput` (or an async validation pattern) for text that must be
checked, such as a workspace URL.

### Cards and rows

`ToolCardShell` is the canonical expandable row for a tool call or agent
step. The collapsed state is boxless: a transcript should read as a
conversation with occasional notes about what ran, not as a stack of
nested panels. Keep the icon small (`size-3.5`), align it with the primary
text line, and keep the title on one line. Expand while work is still
happening, and expand a failed card so the error is one click away; a
settled, successful card collapses to a single row.

`ApprovalCard` is the canonical consent surface. It leads with a short
question, shows the exact action in a muted preview block, and lists
choices as numbered rows ordered narrowest grant first. Destructive or
irreversible actions open a dialog instead; do not use an approval card
for a delete confirmation.

`ChatStatusChip` is the canonical activity summary for a conversation.
It shows live work first, outputs otherwise, and collapses to a compact
pill when a side panel is using the canvas. Do not build a second activity
summary that duplicates outputs, folders, permissions, or agents.

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

### Destructive actions

A destructive action goes through `useConfirm`, which opens the shared
`AlertDialog`: a title that asks the question, a description that names the
consequence, a safe Cancel default, and the `destructive` action variant on
the confirm button. `destructive` is the only chromatic button fill. Do not
add type-to-confirm fields or a second confirmation shape.
