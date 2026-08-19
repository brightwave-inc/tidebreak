import { getPageMarkdown, source } from '@/lib/source';

export const revalidate = false;

export async function GET(
  _req: Request,
  { params }: { params: Promise<{ slug?: string[] }> },
) {
  const { slug } = await params;
  const lookupSlug =
    slug?.length === 1 && slug[0] === 'index' ? undefined : slug;
  const page = source.getPage(lookupSlug);

  return new Response(page ? await getPageMarkdown(page) : '', {
    headers: { 'Content-Type': 'text/plain; charset=utf-8' },
  });
}

export function generateStaticParams() {
  return source.getPages().map((page) => ({
    slug: page.slugs.length > 0 ? page.slugs : ['index'],
  }));
}
