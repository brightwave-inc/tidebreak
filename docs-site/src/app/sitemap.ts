import type { MetadataRoute } from 'next';
import { source } from '@/lib/source';
import { PUBLIC_DOCS_URL } from '@/lib/site';

export const dynamic = 'force-static';

export default function sitemap(): MetadataRoute.Sitemap {
  return source.getPages().map((page) => ({
    url: new URL(
      page.slugs.length > 0 ? `${page.slugs.join('/')}/` : '',
      PUBLIC_DOCS_URL,
    ).toString(),
  }));
}
