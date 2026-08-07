import { getAllPages, getPageRawMarkdown } from '@/lib/content';

export const revalidate = false;

export async function GET() {
  const pages = getAllPages();
  const content = pages.map(getPageRawMarkdown).join('\n\n');

  return new Response(content);
}
