'use client';

import { ThemeProvider } from 'next-themes';
import { SearchDialog } from '@/components/search';
import type { ReactNode } from 'react';

export function Provider({ children }: { children: ReactNode }) {
  return (
    <ThemeProvider attribute="class" defaultTheme="system" enableSystem>
      {children}
      <SearchDialog />
    </ThemeProvider>
  );
}
