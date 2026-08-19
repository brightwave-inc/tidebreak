import { getMDXComponents } from '@/mdx-components';
import { getPageImage, source } from '@/lib/source';
import { PUBLIC_DOCS_URL, REPO_URL } from '@/lib/site';
import {
  DocsBody,
  DocsDescription,
  DocsPage,
  DocsTitle,
  ViewOptionsPopover,
} from 'fumadocs-ui/layouts/docs/page';
import { createRelativeLink } from 'fumadocs-ui/mdx';
import { ArrowRight, Download } from 'lucide-react';
import type { Metadata } from 'next';
import Link from 'next/link';
import { notFound } from 'next/navigation';

export default async function Page(props: PageProps<'/[[...slug]]'>) {
  const params = await props.params;
  const page = source.getPage(params.slug);
  if (!page) notFound();

  const MDX = page.data.body;
  const basePath = process.env.BASE_PATH ?? '';
  const markdownUrl = `${basePath}/llms-mdx/${page.slugs.join('/') || 'index'}`;
  const githubUrl = `${REPO_URL}/blob/main/docs-site/content/docs/${page.path}`;
  const isHome = page.slugs.length === 0;

  return (
    <DocsPage
      toc={page.data.toc}
      className={isHome ? 'tidebreak-doc-home' : undefined}
      tableOfContent={{ style: 'clerk' }}
      tableOfContentPopover={{ style: 'clerk' }}
    >
      <div className="docs-heading-block">
        <div className="docs-heading-copy">
          <p className="docs-eyebrow">
            {isHome ? 'Local-first agentic work' : 'Tidebreak documentation'}
          </p>
          <DocsTitle>{page.data.title}</DocsTitle>
          <DocsDescription>{page.data.description}</DocsDescription>
        </div>
        <ViewOptionsPopover
          markdownUrl={markdownUrl}
          githubUrl={githubUrl}
          className="docs-view-options"
        />
      </div>

      {isHome && (
        <div className="docs-hero-actions not-prose">
          <Link className="docs-primary-action" href="/quickstart">
            Start with the quickstart
            <ArrowRight aria-hidden="true" />
          </Link>
          <a
            className="docs-secondary-action"
            href="https://www.tidebreak.io/download"
          >
            <Download aria-hidden="true" />
            Download Tidebreak
          </a>
        </div>
      )}

      <DocsBody>
        <MDX
          components={getMDXComponents({
            a: createRelativeLink(source, page),
          })}
        />
      </DocsBody>
    </DocsPage>
  );
}

export function generateStaticParams() {
  return source.generateParams();
}

export async function generateMetadata(
  props: PageProps<'/[[...slug]]'>,
): Promise<Metadata> {
  const params = await props.params;
  const page = source.getPage(params.slug);
  if (!page) notFound();

  const publicPath = page.slugs.length > 0
    ? `${page.slugs.join('/')}/`
    : '';

  return {
    title: page.data.title,
    description: page.data.description,
    alternates: {
      canonical: new URL(publicPath, PUBLIC_DOCS_URL).toString(),
    },
    openGraph: {
      images: getPageImage(page).url,
    },
  };
}
