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
