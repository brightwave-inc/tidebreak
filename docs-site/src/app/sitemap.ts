import type { MetadataRoute } from 'next';
import { getAllPages } from '@/lib/content';
import { PUBLIC_DOCS_URL } from '@/lib/site';

export const dynamic = 'force-static';

export default function sitemap(): MetadataRoute.Sitemap {
  return getAllPages().map((page) => ({
    url: new URL(
      page.slugs.length > 0 ? `${page.slugs.join('/')}/` : '',
      PUBLIC_DOCS_URL,
    ).toString(),
  }));
}
