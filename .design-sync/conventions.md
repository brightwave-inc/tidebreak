# Building with Tidebreak's components

Tidebreak is a desktop app for supervising AI coding sessions. Its parts are
compiled app components, not a standalone kit — follow these rules and your
designs will match the product exactly.

## Setup

No provider or wrapper is required. Components are fully styled by the shipped
stylesheet (`styles.css` → `_ds_bundle.css`). The page base is 14px Inter on
`--page-background`; content regions sit on `--background`. Dark mode: add the
`dark` class to the document root — every token flips.

## The styling idiom

Components style themselves — pick looks via props, never by re-styling their
internals:

- `Button`: `variant` = default | secondary | outline | ghost | destructive |
  ghost-destructive | link; `size` = default | sm | lg | xs | 2xs | icon |
  icon-sm | icon-xs.
- `Badge`: `variant` = default | secondary | destructive | outline | success |
  warning | critical | info; `size` = default | sm.
- `Input` `size` = default | sm. There is no error variant on Input/Textarea.

For your own layout glue, know that **the stylesheet is a compiled snapshot**:
only utility classes already present in `_ds_bundle.css` resolve. These
families are all present and safe: layout (`flex`, `grid`, `gap-1/2/3`,
`items-center`, `justify-between`, `min-w-0`, `flex-1`, `truncate`,
`overflow-hidden`, `p-2/3/4`, `px-*`, `py-*`, `mt-*`, `rounded-md`,
`rounded-lg`, `border`, `border-b`, `shrink-0`), type (`text-xs`, `text-sm`,
`text-2xs`, `font-medium`, `font-mono`, `tabular-nums`), and color utilities
bound to the theme tokens:

- Surfaces: `bg-background`, `bg-page-background`, `bg-muted`, `bg-card`,
  `bg-popover`, `bg-secondary`, `border-border`.
- Ink: `text-foreground`, `text-muted-foreground`, `text-primary`,
  `text-primary-foreground`, `text-secondary-foreground`.
- Status (the semantic quads, one per tone — success | warning | critical |
  info): `text-<tone>`, `text-<tone>-foreground`,
  `text-<tone>-foreground-muted`, `bg-<tone>-background`,
  `border-<tone>-border`. Example: `bg-warning-background
  border-warning-border text-warning-foreground`.

When a class you want may not exist, use an inline `style` instead of inventing
a class name. Raw tokens are also available as CSS variables: `var(--mono)`,
`var(--sans)`, `var(--radius)`, `var(--code-block-bg)`, plus every color token
above (`var(--success-background)`, …).

## Where the truth lives

Read `styles.css` and its imports for the full token set; each component's
`.d.ts` is its props contract and its `.prompt.md` shows verified compositions.
Mono content (commands, branches, paths) is always `font-mono text-xs` or
`var(--mono)` — that's the app's strongest visual signature.

## Idiomatic example

```tsx
import { Badge, Button, Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "tidebreak-desktop-ui";

<div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
  <div className="flex items-center justify-between">
    <h2 className="text-sm font-medium">Workspaces</h2>
    <Button size="sm">New workspace</Button>
  </div>
  <Table>
    <TableHeader>
      <TableRow>
        <TableHead>Workspace</TableHead><TableHead>Branch</TableHead><TableHead>Status</TableHead>
      </TableRow>
    </TableHeader>
    <TableBody>
      <TableRow>
        <TableCell>Fix flaky retry test</TableCell>
        <TableCell className="font-mono text-xs">tb/fix-retry-test</TableCell>
        <TableCell><Badge variant="success" size="sm">PR open</Badge></TableCell>
      </TableRow>
    </TableBody>
  </Table>
</div>
```
