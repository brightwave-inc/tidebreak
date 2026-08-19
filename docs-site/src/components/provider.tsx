'use client';

import { RootProvider } from 'fumadocs-ui/provider/next';
import type { ReactNode } from 'react';

export function Provider({
  children,
  searchApi,
}: {
  children: ReactNode;
  searchApi: string;
}) {
  return (
    <RootProvider
      search={{
        options: {
          type: 'static',
          api: searchApi,
        },
      }}
      theme={{
        attribute: 'class',
        defaultTheme: 'system',
        enableSystem: true,
      }}
    >
      {children}
    </RootProvider>
  );
}
