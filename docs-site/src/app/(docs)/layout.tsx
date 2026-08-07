import { getSidebar } from '@/lib/content';
import { DocsShell } from '@/components/docs-layout';

export default function Layout({ children }: LayoutProps<'/'>) {
  const sections = getSidebar();

  return <DocsShell sections={sections}>{children}</DocsShell>;
}
