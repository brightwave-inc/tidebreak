import { getPage, getPageRawMarkdown, getAllPages } from '@/lib/content';

export const revalidate = false;

export async function GET(
  _req: Request,
  { params }: { params: Promise<{ slug?: string[] }> },
) {
  const { slug } = await params;
  const lookupSlug =
    slug?.length === 1 && slug[0] === 'index' ? undefined : slug;
  const page = getPage(lookupSlug);

  return new Response(page ? getPageRawMarkdown(page) : '', {
    headers: { 'Content-Type': 'text/plain; charset=utf-8' },
  });
}

export function generateStaticParams() {
  const pages = getAllPages();
  // `output: export` requires at least one param even before any content
  // exists; the stub route then serves an empty document.
  if (pages.length === 0) return [{ slug: ['index'] }];

  return pages.map((page) => ({
    slug: page.slugs.length > 0 ? page.slugs : ['index'],
  }));
}
