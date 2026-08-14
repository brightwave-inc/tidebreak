'use client';

import Link from 'next/link';
import { usePathname } from 'next/navigation';
import { cn } from '@/lib/utils';
import type { SidebarSection } from '@/lib/content';
import { X } from 'lucide-react';
import { useEffect } from 'react';
import { Button } from '@/components/ui/button';

const PRODUCT_URL = 'https://www.tidebreak.sh';
const REPO_URL = 'https://github.com/brightwave-inc/tidebreak';

function SidebarNav({ sections, onNavigate }: { sections: SidebarSection[]; onNavigate?: () => void }) {
  const pathname = usePathname();

  function isActive(url: string) {
    const normalized = pathname.replace(/\/$/, '') || '/';
    const target = url.replace(/\/$/, '') || '/';
    return normalized === target;
  }

  return (
    <nav className="space-y-6">
      {sections.map((section) => (
        <div key={section.title}>
          {section.title && (
            <p className="mb-2 px-3 text-[11px] font-bold uppercase tracking-[0.08em] text-muted-foreground">
              {section.title}
            </p>
          )}
          <ul className="space-y-px">
            {section.pages.map((page) => (
              <li key={page.slug}>
                <Link
                  href={page.url}
                  onClick={onNavigate}
                  className={cn(
                    'block rounded-md px-3 py-1.5 text-[13px] transition-colors',
                    isActive(page.url)
                      ? 'bg-accent font-semibold text-foreground'
                      : 'text-muted-foreground hover:bg-accent/50 hover:text-foreground',
                  )}
                >
                  {page.title}
                </Link>
              </li>
            ))}
          </ul>
        </div>
      ))}
    </nav>
  );
}

export function Sidebar({ sections }: { sections: SidebarSection[] }) {
  return (
    <aside className="hidden w-56 shrink-0 lg:block" data-sidebar>
      <div className="sticky top-[3.75rem] h-[calc(100vh-3.75rem)] overflow-y-auto py-5 pr-2 font-[550]">
        <SidebarNav sections={sections} />
      </div>
    </aside>
  );
}

export function MobileSidebar({
  sections,
  open,
  onClose,
}: {
  sections: SidebarSection[];
  open: boolean;
  onClose: () => void;
}) {
  useEffect(() => {
    if (!open) return;
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') onClose();
    }
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <>
      <div
        className="fixed inset-0 z-40 bg-black/50 lg:hidden"
        onClick={onClose}
      />
      <aside
        className="fixed inset-y-0 left-0 z-50 flex w-72 flex-col overflow-y-auto border-r border-border bg-page-background p-5 shadow-lg lg:hidden"
        data-sidebar
        role="dialog"
        aria-modal="true"
        aria-label="Documentation navigation"
      >
        <div className="mb-5 flex items-center justify-between">
          <p className="text-sm font-semibold">Tidebreak Docs</p>
          <Button variant="ghost" size="icon-sm" onClick={onClose} aria-label="Close navigation">
            <X className="h-4 w-4" />
          </Button>
        </div>
        <div className="font-[550]">
          <SidebarNav sections={sections} onNavigate={onClose} />
        </div>
        <nav className="mt-8 flex gap-4 border-t border-border pt-4 text-sm text-muted-foreground" aria-label="More links">
          <a href={PRODUCT_URL} className="hover:text-foreground">Product</a>
          <a href={REPO_URL} className="hover:text-foreground">GitHub</a>
        </nav>
      </aside>
    </>
  );
}
