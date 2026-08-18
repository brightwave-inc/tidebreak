# design-sync notes — Tidebreak

Repo-specific facts a future sync needs. The config is at
`.design-sync/config.json`; the target project is "Tidebreak"
(9f72d042-a029-48e3-b53c-14d4dad2a3be).

- **This is an app, not a library.** No dist entry; the sync entry is the
  committed `crates/tidebreak-desktop/ui/src/design-system.ts`, passed as
  `--entry`. Keep that file in step with the curated `componentSrcMap` — a
  component missing from the entry ships a card but is `undefined` on
  `window.Tidebreak`.
- **Converter invocation** (from repo root):
  `node .ds-sync/package-build.mjs --config .design-sync/config.json
  --node-modules crates/tidebreak-desktop/ui/node_modules
  --entry crates/tidebreak-desktop/ui/src/design-system.ts --out ./ds-bundle`.
  Omitting `--entry` crashes: the package is private, so
  `node_modules/tidebreak-desktop-ui` never exists.
- **CSS comes from the app build.** `buildCmd` runs `pnpm build` (in
  `crates/tidebreak-desktop/ui`), copies the hashed vite stylesheet to
  `.ds-compiled.css`, then perl-rewrites font urls: `/assets/KaTeX_*-<hash>.*`
  → `node_modules/katex/dist/fonts/...` and `/fonts/inter/` →
  `public/fonts/inter/`. Without the rewrite the harvested
  `fonts/fonts.css` carries dead absolute urls ([FONT_DANGLING]).
- **Playwright**: repo has no playwright dep. The machine cache
  (`~/Library/Caches/ms-playwright`, chromium-1234) pairs with
  playwright@1.62.0, installed into `.ds-sync/` only.
- **Tokens live in `src/styles.css`** (`:root`/`.dark`, oklch); previews render
  the light theme. Cards inherit real component styling through the compiled
  CSS — no provider wrapper is needed for the primitives (verified: Button,
  Table, MessageMarkdown render fully styled with no wrapper).
- Overlay primitives (Dialog, AlertDialog, DropdownMenu, Select, Popover,
  Tooltip, Toaster) are pinned `cardMode: single` with viewports in config;
  Table and ResizablePanelGroup are `column`.

## Known render warns

- **A GLOBAL config edit (e.g. `extraEntries`) re-mints every component's grade
  key** — expect a full re-grade pass after one, even for unchanged previews.
- **A component-level cardMode/viewport override re-keys that component's grade
  slice** (`viewport` and `skip` are hashed; `cardMode`/`primaryStory` are
  not). After editing `overrides`, run one FULL `package-build.mjs` before any
  scoped preview-rebuild, or every override-carrying component fails
  `[CONFIG_STALE]` — and a scoped rebuild fails its whole batch on one stale
  name (bisect, then batch the survivors).
- **Sidebar rail previews show the expanded width only.** `Sidebar`/
  `SidebarButton` read `useSidebarWidth()` → `useUiStore` (`src/UiStore.ts`),
  which the design-system entry doesn't re-export and which is one store per
  page — cells can't flip `sidebarCollapsed`. Accepted; to ship a compact-rail
  card, re-export the store from `src/design-system.ts` or add a width prop.
- **HarnessPicker cards show the closed trigger only.** The branded rows live
  in a portalled Radix `SelectContent` the component gives no way to force
  open (no defaultOpen passthrough). Accepted; fixing it means adding an
  open-state escape hatch to the component itself.
- **AttentionBadge `working` state renders `null` by design** — the cell is
  labeled so the blank row reads as intentional.

## Preview-authoring facts (survive refactors poorly — revisit on breakage)

- **Hover-only paint is invisible on a static sheet.** MessageFooter's time,
  InlineCitation's brand-accent band, ClipboardCopyButton's hover background:
  pin the open values with a scoped `ds-`-prefixed class + inline `<style>` in
  ONE cell, keep another at rest. (Technique used in the committed previews.)
- `InlineCitation` silently renders bare children unless documentId AND
  stringified locator match a provider source and `onOpenSource` is set; its
  ordinal is `sr-only` (chip shows a Quote glyph — not incomplete).
- `AssistantWorkingIndicator` without `compacting` = lone pulsing logomark +
  `sr-only` label; near-blank cell is correct.
- `ChangeSummaryCard` needs only a stub client with three async methods; file
  rows are wire-shaped (`snapshot_id`, `classification`, `undo`, …).
- `TurnFailureNotice` drops the model-attribution line when `model` is omitted.
- `ContextUsageIndicator` numbers live in a hover tooltip — pair rings with
  captions.
- `Sidebar` sizes with `flex-basis`: cells must wrap it in a `display: flex`
  parent with explicit height or it collapses.
- `SidebarButton` label span needs `flex: 1; min-width: 0` for truncation.
- `AttentionBadge` `working` state and `ToolStatusIcon` `completed` render
  `null` by design; `DomainFavicon` always draws the local globe (privacy) —
  identical rows are correct.
- `ToolActivityGroup`'s rail and `ToolCommandCard`'s settled body are
  unreachable statically (expand state is internal / running-only); cells
  exercise phase labels and running bodies instead. `animate={false}` required
  on `ToolActivityGroup` or the typewriter half-types in captures.
- `Input`/`Textarea` have no error variant (no aria-invalid styling); disabled
  is the only non-default static state. `Input` `size` is "default"|"sm" only.
- `SearchInput` is fully controlled (`value`/`onValueChange`); `OptionListbox`
  is a bare `ul` — previews wrap it in the popover classes the app uses.
- Typed prop shapes for chat cards come from `src/api/types.ts` (camelCase),
  not `src/generated/wire.ts` (snake_case).
- Overlay capture rules: Radix portals mount on `document.body` so open
  overlays center in the configured viewport; `modal={false}` on Dialog
  deletes the scrim (only DropdownMenu needs it); `DropdownMenuSub` needs
  `open` (not `defaultOpen`); Tooltip needs `TooltipProvider delayDuration=0`
  + `open` (renders instant-open, skipping the entry animation). `sonner`
  keeps a module-scope toast store — `extraEntries: ["sonner"]` shims preview
  imports to the bundle's copy; without it Toaster cells capture blank.
- **`cardMode: single` picks the alphabetically-first export unless
  `primaryStory` is set** — export order in the .tsx does not matter. Every
  single-mode component in `overrides` now pins `primaryStory` explicitly;
  keep doing that for any new one.
- `DropdownMenu` exports no Label/Shortcut/RadioItem; `CardFooter` stretches a
  lone child (wrap a bare Badge in a div); `AlertDialogAction` takes Button's
  `variant`.
- Radix ScrollArea lays its viewport child out as a table: a truncating list
  inside needs an explicit width on the content wrapper — `minWidth: 0` +
  `textOverflow` alone do nothing.
- `ResizableHandle` has no `withHandle` grip prop; the 1px hairline between
  panels is the real component.
- Wire-shaped preview props (Attention, Diffstat, HarnessDoctorEntry,
  ToolDetail) are copied from the app's test fixtures; a wire refactor breaks
  the preview rebuild first, which names the file.

## Re-sync risks

- `src/design-system.ts` and `componentSrcMap` must move together; adding a
  shared component to the app without touching either leaves it out of the
  design project silently.
- The `.ds-compiled.css` snapshot embeds hashed KaTeX names via the perl
  rewrite; a katex version bump changes the font set — re-run `buildCmd`, do
  not reuse a stale `.ds-compiled.css`.
- Preview compositions imitate app usage (MessageList wrappers, wire-shaped
  props). Refactors to those prop shapes will break authored previews at
  preview-rebuild time — the compile failure names the file.
- Playwright/chromium pairing is machine-local; on a new machine check
  `~/Library/Caches/ms-playwright` (or `~/.cache/ms-playwright` on Linux) and
  install the matching playwright into `.ds-sync/`.
