import { MDXRemote } from 'next-mdx-remote/rsc';
import remarkGfm from 'remark-gfm';
import { getMDXComponents } from '@/mdx-components';
import {
  getPage,
  getPageImage,
  getPageNav,
  extractToc,
  stripImports,
  generateStaticParams as genParams,
} from '@/lib/content';
import { TableOfContents } from '@/components/toc';
import { PageNavigation } from '@/components/page-nav';
import type { Metadata } from 'next';

/** Shown by the bare scaffold, before any MDX has been written. */
function EmptyState() {
  return (
    <main className="min-w-0 flex-1 py-8 lg:py-10">
      <div className="mx-auto max-w-3xl px-6">
        <h1 className="text-3xl font-bold tracking-tight text-foreground">
          Tidebreak Docs
        </h1>
        <p className="mt-2 text-lg text-muted-foreground">
          No documentation pages have been added yet. Add MDX files under{' '}
          <code>content/docs/</code> and list them in{' '}
          <code>content/docs/meta.json</code>.
        </p>
      </div>
    </main>
  );
}

export default async function Page(props: PageProps<'/[[...slug]]'>) {
  const params = await props.params;
  const page = getPage(params.slug);
  if (!page) return <EmptyState />;

  const toc = extractToc(page.rawContent);
  const nav = getPageNav(page);
  const markdownUrl = `${process.env.__NEXT_ROUTER_BASEPATH || ''}/llms-mdx/${page.slugs.join('/') || 'index'}`;
  const source = stripImports(page.rawContent);

  return (
    <>
      <main className="min-w-0 flex-1 py-8 lg:py-10">
        <div className="mx-auto max-w-3xl px-6">
          <h1 className="text-3xl font-bold tracking-tight text-foreground">
            {page.title}
          </h1>
          {page.description && (
            <p className="mt-2 text-lg text-muted-foreground">
              {page.description}
            </p>
          )}
          <div className="mt-2 mb-6">
            <a
              href={markdownUrl}
              className="text-xs text-muted-foreground underline underline-offset-2 transition-colors hover:text-foreground"
            >
              View as Markdown
            </a>
          </div>
          <article className="prose prose-neutral dark:prose-invert max-w-none">
            <MDXRemote
              source={source}
              components={getMDXComponents()}
              options={{
                mdxOptions: {
                  remarkPlugins: [remarkGfm],
                },
              }}
            />
          </article>
          <PageNavigation prev={nav.prev} next={nav.next} />
        </div>
      </main>
      <TableOfContents entries={toc} />
    </>
  );
}

export function generateStaticParams() {
  const params = genParams();
  // `output: export` needs at least one param; the empty scaffold still
  // renders its index route.
  return params.length > 0 ? params : [{ slug: [] as string[] }];
}

export async function generateMetadata(
  props: PageProps<'/[[...slug]]'>,
): Promise<Metadata> {
  const params = await props.params;
  const page = getPage(params.slug);
  if (!page) {
    return {
      title: { absolute: 'Tidebreak Docs' },
      description: 'Documentation for Tidebreak.',
      alternates: { canonical: 'https://tidebreak.sh/docs/' },
    };
  }

  const publicPath = page.slugs.length > 0
    ? `${page.slugs.join('/')}/`
    : '';

  return {
    title: page.title,
    description: page.description,
    alternates: {
      canonical: `https://tidebreak.sh/docs/${publicPath}`,
    },
    openGraph: {
      images: getPageImage(page).url,
    },
  };
}
