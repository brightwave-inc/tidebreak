# Tidebreak Docs

The user documentation for Tidebreak. It is a Fumadocs site on the Next.js App
Router, built as a static export and published at
<https://www.tidebreak.io/docs/>.

## Documentation boundaries

- `content/docs/` teaches people how to install, configure, and use Tidebreak.
- The repository root [`README.md`](../README.md) is a short project and
  contributor entry point; it should link here instead of reproducing guides.
- [`docs/`](../docs) holds maintainer architecture, contracts, operations,
  plans, and decision records.
- Product positioning and launch copy belong to the separate
  [`brightwave-inc/tidebreak-site`](https://github.com/brightwave-inc/tidebreak-site)
  marketing repository.

When a subject matters to both users and maintainers, write each page for its
audience and link across the boundary. Do not keep two near-identical versions
of the same guide.

## Local development

```sh
pnpm install
pnpm dev
```

The site is served at <http://localhost:3000/>. Prose lives in
`content/docs/*.mdx`; Fumadocs compiles the content, builds the table of
contents and search data, and reads sidebar order and section headings from
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

`pnpm build` emits the static Fumadocs search index at `/api/search/` alongside
the exported pages. The Vercel output keeps `/search-index.json` as a legacy
deployment-health alias to that same file.

`pnpm types:check` type-checks against the route types Next generates under
`.next/`, so run `pnpm dev` or `pnpm build` at least once in a fresh clone
before it will pass.

## Layout

| Path | What lives there |
| --- | --- |
| `content/docs/` | MDX pages and `meta.json` (sidebar order) |
| `src/app/` | Routes, Fumadocs layouts, and `global.css` design tokens |
| `src/components/` | Tidebreak branding and the Fumadocs provider |
| `src/lib/source.ts` | Fumadocs content collection and source loader |
| `src/lib/layout.shared.tsx` | Shared navigation and external links |

## Serving under a subpath

This site is meant to be served under a path on the marketing site rather than
at the root of its own origin, so it must be built with `BASE_PATH` set to that
path:

```sh
BASE_PATH=/docs pnpm build
```

`next.config.mjs` passes `BASE_PATH` through to Next's `basePath`, which
rewrites asset URLs, `next/link` hrefs, and the search-index fetch. Leave it
unset for local development. See `.env.example`.

The release workflow deploys this export to the dedicated documentation
project, then the marketing project serves it under
`https://www.tidebreak.io/docs/`. Canonical metadata and the sitemap point at
that public path so the raw deployment origin is not indexed as a separate
site.

## Design tokens

`src/app/global.css` maps Fumadocs' surface tokens onto the marketing site's
Geist typography, cool neutral ramp, restrained brand blue, and light/dark
surface system. Keep those tokens aligned with
`tidebreak-site/src/styles/globals.css` when the public brand changes.
