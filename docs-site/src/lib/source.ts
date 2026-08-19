import { loader } from 'fumadocs-core/source';
import { defineDocs } from 'fumadocs-mdx/macro';

const docs = defineDocs({
  dir: 'content/docs',
  docs: {
    postprocess: {
      includeProcessedMarkdown: true,
    },
  },
});

export const source = loader({
  baseUrl: '/',
  source: docs.toFumadocsSource(),
});

export type DocsPage = ReturnType<typeof source.getPages>[number];

export async function getPageMarkdown(page: DocsPage): Promise<string> {
  const content = (await page.data.getText('processed')).trim();
  return `# ${page.data.title} (${page.url})\n\n${content}`.trim();
}

export function getPageImage(page: DocsPage) {
  const segments = [...page.slugs, 'image.png'];
  return {
    segments,
    url: `/docs/og/${segments.join('/')}`,
  };
}
