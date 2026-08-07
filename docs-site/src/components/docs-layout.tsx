'use client';

import { useState, useCallback } from 'react';
import type { ReactNode } from 'react';
import { Header } from './header';
import { Sidebar, MobileSidebar } from './sidebar';
import type { SidebarSection } from '@/lib/content';
import { Button } from '@/components/ui/button';
import { PanelLeft } from 'lucide-react';

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
      <Header onOpenSearch={openSearch} />
      <div className="mx-auto flex w-full max-w-screen-2xl flex-1 gap-0 px-2 py-2 lg:px-3">
        <Sidebar sections={sections} />
        <Button
          variant="outline"
          size="icon-sm"
          className="fixed bottom-4 left-4 z-30 rounded-full shadow-md lg:hidden"
          onClick={() => setSidebarOpen(true)}
          aria-label="Open sidebar"
        >
          <PanelLeft className="h-4 w-4" />
        </Button>
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
