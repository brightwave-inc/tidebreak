import { source } from '@/lib/source';

export const revalidate = false;

export async function GET() {
  const pages = source.getPages();
  const basePath = process.env.BASE_PATH ?? '';

  const lines = [
    '# Tidebreak Docs',
    '',
    '> Documentation for Tidebreak.',
    '',
    '## Pages',
    '',
    ...pages.map((page) => {
      const mdUrl = `${basePath}/llms-mdx/${page.slugs.join('/') || 'index'}`;
      return `- [${page.data.title}](${mdUrl})${page.data.description ? `: ${page.data.description}` : ''}`;
    }),
  ];

  return new Response(lines.join('\n'), {
    headers: { 'Content-Type': 'text/plain' },
  });
}
