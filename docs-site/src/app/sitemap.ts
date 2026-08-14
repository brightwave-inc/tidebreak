import type { MetadataRoute } from 'next';
import { getAllPages } from '@/lib/content';

const PUBLIC_DOCS_URL = 'https://tidebreak.sh/docs/';

export const dynamic = 'force-static';

export default function sitemap(): MetadataRoute.Sitemap {
  return getAllPages().map((page) => ({
    url: new URL(
      page.slugs.length > 0 ? `${page.slugs.join('/')}/` : '',
      PUBLIC_DOCS_URL,
    ).toString(),
  }));
}
