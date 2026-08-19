import { source } from '@/lib/source';
import { DocsLayout } from 'fumadocs-ui/layouts/docs';
import { baseOptions } from '@/lib/layout.shared';

export default function Layout({ children }: LayoutProps<'/'>) {
  return (
    <DocsLayout
      tree={source.getPageTree()}
      {...baseOptions()}
      sidebar={{
        collapsible: true,
        footer: (
          <p
            key="tidebreak-sidebar-note"
            className="px-1 text-xs leading-5 text-fd-muted-foreground"
          >
            Local-first. Open source. Pre-1.0.
          </p>
        ),
      }}
    >
      {children}
    </DocsLayout>
  );
}
