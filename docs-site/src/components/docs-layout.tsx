'use client';

import { useState, useCallback } from 'react';
import type { ReactNode } from 'react';
import { Header } from './header';
import { Sidebar, MobileSidebar } from './sidebar';
import type { SidebarSection } from '@/lib/content';

export function DocsShell({
  sections,
  children,
}: {
  sections: SidebarSection[];
  children: ReactNode;
}) {
  const [sidebarOpen, setSidebarOpen] = useState(false);

  const openSearch = useCallback(() => {
    window.dispatchEvent(new CustomEvent('open-search'));
  }, []);

  return (
    <>
      <a
        href="#main-content"
        className="fixed left-4 top-3 z-[60] -translate-y-20 rounded-md bg-foreground px-3 py-2 text-sm font-medium text-background transition-transform focus:translate-y-0"
      >
        Skip to content
      </a>
      <Header
        onOpenSearch={openSearch}
        onOpenSidebar={() => setSidebarOpen(true)}
      />
      <div className="mx-auto flex w-full max-w-screen-2xl flex-1 gap-0 px-2 py-2 lg:px-3">
        <Sidebar sections={sections} />
        <MobileSidebar
          sections={sections}
          open={sidebarOpen}
          onClose={() => setSidebarOpen(false)}
        />
        <div className="min-w-0 flex-1 rounded-lg border border-border bg-background">
          <div className="flex">
            {children}
          </div>
        </div>
      </div>
    </>
  );
}
