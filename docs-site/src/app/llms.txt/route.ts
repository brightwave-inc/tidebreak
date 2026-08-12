import { getAllPages } from '@/lib/content';

export const revalidate = false;

export async function GET() {
  const pages = getAllPages();
  const basePath = process.env.__NEXT_ROUTER_BASEPATH || '';

  const lines = [
    '# Tidebreak Docs',
    '',
    '> Documentation for Tidebreak.',
    '',
    '## Pages',
    '',
    ...pages.map((page) => {
      const mdUrl = `${basePath}/llms-mdx/${page.slugs.join('/') || 'index'}`;
      return `- [${page.title}](${mdUrl})${page.description ? `: ${page.description}` : ''}`;
    }),
  ];

  return new Response(lines.join('\n'), {
    headers: { 'Content-Type': 'text/plain' },
  });
}
