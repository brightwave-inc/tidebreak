import fs from 'node:fs';
import path from 'node:path';
import matter from 'gray-matter';

const CONTENT_DIR = path.join(process.cwd(), 'content', 'docs');

export interface PageData {
  slug: string;
  slugs: string[];
  url: string;
  title: string;
  description: string;
  rawContent: string;
}

export interface SidebarSection {
  title: string;
  pages: { slug: string; title: string; url: string }[];
}

export interface TocEntry {
  depth: number;
  text: string;
  id: string;
}

function slugToFilePath(slug?: string): string {
  const filename = slug || 'index';
  return path.join(CONTENT_DIR, `${filename}.mdx`);
}

function parsePageFile(filePath: string): PageData | null {
  if (!fs.existsSync(filePath)) return null;

  const raw = fs.readFileSync(filePath, 'utf-8');
  const { data, content } = matter(raw);
  const basename = path.basename(filePath, '.mdx');
  const slug = basename === 'index' ? '' : basename;
  const slugs = slug ? [slug] : [];

  return {
    slug,
    slugs,
    url: slug ? `/${slug}` : '/',
    title: data.title ?? basename,
    description: data.description ?? '',
    rawContent: content,
  };
}

let _allPages: PageData[] | null = null;

export function getAllPages(): PageData[] {
  // Skip cache in dev so content changes trigger hot reload
  if (_allPages && process.env.NODE_ENV !== 'development') return _allPages;

  // The scaffold must run before any content exists, so an absent or empty
  // content directory is a valid state rather than a build failure.
  if (!fs.existsSync(CONTENT_DIR)) return [];

  const files = fs.readdirSync(CONTENT_DIR).filter((f) => f.endsWith('.mdx'));
  _allPages = files
    .map((f) => parsePageFile(path.join(CONTENT_DIR, f)))
    .filter((p): p is PageData => p !== null);

  return _allPages;
}

export function getPage(slug?: string[]): PageData | null {
  const s = slug?.join('/') ?? '';
  return getAllPages().find((p) => p.slug === s) ?? null;
}

export function getPageRawMarkdown(page: PageData): string {
  return `# ${page.title} (${page.url})\n\n${stripMdxComments(page.rawContent)}`;
}

/**
 * Removes MDX and HTML comments. The MDX renderer drops these itself, but the
 * raw-markdown routes serve the source, where an authoring note would
 * otherwise be published as text. Kept in sync with the equivalent strip in
 * scripts/generate-search-index.mjs.
 */
export function stripMdxComments(rawContent: string): string {
  return rawContent
    .replace(/\{\s*\/\*[\s\S]*?\*\/\s*\}/g, '')
    .replace(/<!--[\s\S]*?-->/g, '');
}

export function getSidebar(): SidebarSection[] {
  const metaPath = path.join(CONTENT_DIR, 'meta.json');
  if (!fs.existsSync(metaPath)) return [];

  let meta: { pages?: string[] };
  try {
    meta = JSON.parse(fs.readFileSync(metaPath, 'utf-8'));
  } catch {
    // A half-written meta.json shouldn't take the whole site down.
    return [];
  }
  if (!Array.isArray(meta.pages)) return [];

  const pages = getAllPages();
  const sections: SidebarSection[] = [];
  let current: SidebarSection = { title: '', pages: [] };

  for (const entry of meta.pages) {
    const sectionMatch = entry.match(/^---(.+)---$/);
    if (sectionMatch) {
      if (current.pages.length > 0 || current.title) {
        sections.push(current);
      }
      current = { title: sectionMatch[1], pages: [] };
    } else {
      const page = pages.find((p) => (p.slug || 'index') === entry);
      if (page) {
        current.pages.push({
          slug: page.slug,
          title: page.title,
          url: page.url,
        });
      }
    }
  }

  if (current.pages.length > 0 || current.title) {
    sections.push(current);
  }

  return sections;
}

export function stripImports(rawContent: string): string {
  return rawContent
    .replace(/^import\s+\{[^}]*\}\s+from\s+['"][^'"]+['"];?\s*$/gm, '')
    .replace(/^import\s+.*from\s+['"][^'"]+['"];?\s*$/gm, '');
}

export function extractToc(rawContent: string): TocEntry[] {
  const entries: TocEntry[] = [];
  const lines = rawContent.split('\n');

  for (const line of lines) {
    const match = line.match(/^(#{2,4})\s+(.+)$/);
    if (match) {
      const depth = match[1].length;
      const text = match[2].trim();
      const id = text
        .toLowerCase()
        .replace(/[^\w\s-]/g, '')
        .replace(/\s+/g, '-');
      entries.push({ depth, text, id });
    }
  }

  return entries;
}

export interface PageNav {
  prev: { title: string; description: string; url: string } | null;
  next: { title: string; description: string; url: string } | null;
}

export function getPageNav(page: PageData): PageNav {
  const sidebar = getSidebar();
  const allSidebarPages = sidebar.flatMap((s) => s.pages);
  const idx = allSidebarPages.findIndex((p) => p.slug === page.slug);

  return {
    prev: idx > 0
      ? { ...allSidebarPages[idx - 1], description: getAllPages().find((p) => p.slug === allSidebarPages[idx - 1].slug)?.description ?? '' }
      : null,
    next: idx < allSidebarPages.length - 1
      ? { ...allSidebarPages[idx + 1], description: getAllPages().find((p) => p.slug === allSidebarPages[idx + 1].slug)?.description ?? '' }
      : null,
  };
}

export function getPageImage(page: PageData) {
  const segments = [...page.slugs, 'image.png'];
  return {
    segments,
    url: `/og/${segments.join('/')}`,
  };
}

export function generateStaticParams() {
  return getAllPages().map((page) => ({
    slug: page.slugs,
  }));
}
