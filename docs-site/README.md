# OpenWave Docs

The documentation site for OpenWave. Next.js (App Router) with MDX content,
built as a static export.

## Local development

```sh
pnpm install
pnpm dev
```

The site is served at <http://localhost:3000/>. Prose lives in
`content/docs/*.mdx`; the sidebar order and section headings come from
`content/docs/meta.json`. A page only appears in the sidebar if it is listed
there.

Useful scripts:

| Script | What it does |
| --- | --- |
| `pnpm dev` | Dev server with hot reload |
| `pnpm build` | Static export to `out/` |
| `pnpm start` | Serve the built `out/` directory |
| `pnpm types:check` | `tsc --noEmit` |
| `pnpm lint` | ESLint |

`pnpm dev` and `pnpm build` both regenerate `public/search-index.json` from
`content/docs/` first. That file is generated output and is not committed.

`pnpm types:check` type-checks against the route types Next generates under
`.next/`, so run `pnpm dev` or `pnpm build` at least once in a fresh clone
before it will pass.

## Layout

| Path | What lives there |
| --- | --- |
| `content/docs/` | MDX pages and `meta.json` (sidebar order) |
| `src/app/` | Routes, root layout, `global.css` design tokens |
| `src/components/` | Header, sidebar, search, TOC, MDX components |
| `src/lib/content.ts` | Reads and parses the MDX content tree |
| `scripts/generate-search-index.mjs` | Builds the client-side search index |

## Serving under a subpath

This site is meant to be served under a path on the marketing site rather than
at the root of its own origin, so it must be built with `BASE_PATH` set to that
path:

```sh
BASE_PATH=/openwave/docs pnpm build
```

`next.config.mjs` passes `BASE_PATH` through to Next's `basePath`, which
rewrites asset URLs, `next/link` hrefs, and the search-index fetch. Leave it
unset for local development. See `.env.example`.

Nothing here is wired to a host yet. `metadataBase` in `src/app/layout.tsx` is
set to the intended public URL, but the indexing decision is still open —
either a canonical URL pointing at the public path, or `noindex` on the raw
deployment origin, so the deployment origin and the public URL are not indexed
as separate sites.

## Design tokens

`src/app/global.css` carries the OpenWave palette, kept in step with the
desktop app's `crates/openwave-desktop/ui/src/styles.css`. Dark mode uses cool
near-neutral grays and carries elevation on the lightness ramp alone: page
chrome `0.12` < popover `0.14` < card `0.16` < content surface `0.21`.
