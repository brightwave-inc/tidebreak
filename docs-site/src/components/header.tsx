'use client';

import Link from 'next/link';
import { TidebreakLogo } from './logo';
import { ThemeToggle } from './theme-toggle';
import { SearchTrigger } from './search-trigger';
import { Button } from '@/components/ui/button';
import { Menu, X } from 'lucide-react';
import { useState } from 'react';

const PRODUCT_URL = 'https://tidebreak.io';
const REPO_URL = 'https://github.com/brightwave-inc/openwave';

const navLinks = [{ text: 'Product', href: PRODUCT_URL }];

function GitHubIcon(props: React.ComponentPropsWithoutRef<'svg'>) {
  return (
    <svg viewBox="0 0 16 16" fill="currentColor" aria-hidden="true" {...props}>
      <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27s1.36.09 2 .27c1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8Z" />
    </svg>
  );
}

export function Header({ onOpenSearch }: { onOpenSearch: () => void }) {
  const [mobileOpen, setMobileOpen] = useState(false);

  return (
    <header className="sticky top-0 z-50 border-b border-border bg-background/80 backdrop-blur-lg">
      <div className="mx-auto flex h-14 max-w-screen-2xl items-center gap-4 px-4 lg:px-6">
        <Link href="/" className="mr-2 inline-flex items-center gap-2 shrink-0">
          <TidebreakLogo />
        </Link>

        <nav className="hidden items-center gap-1 md:flex">
          {navLinks.map((link) => (
            <a
              key={link.href}
              href={link.href}
              className="rounded-md px-3 py-1.5 text-sm text-muted-foreground transition-colors hover:text-foreground"
            >
              {link.text}
            </a>
          ))}
        </nav>

        <div className="ml-auto flex items-center gap-2">
          <SearchTrigger onClick={onOpenSearch} />
          <ThemeToggle />
          <div className="hidden items-center gap-2 md:flex">
            <Button variant="outline" size="sm" asChild>
              <a href={REPO_URL} target="_blank" rel="noopener noreferrer">
                <GitHubIcon className="h-4 w-4" />
                GitHub
              </a>
            </Button>
          </div>
          <Button
            variant="ghost"
            size="icon-sm"
            className="md:hidden"
            onClick={() => setMobileOpen(!mobileOpen)}
            aria-label="Toggle menu"
          >
            {mobileOpen ? <X className="h-5 w-5" /> : <Menu className="h-5 w-5" />}
          </Button>
        </div>
      </div>

      {mobileOpen && (
        <div className="border-t border-border bg-background px-4 py-4 md:hidden">
          <nav className="flex flex-col gap-2">
            {navLinks.map((link) => (
              <a
                key={link.href}
                href={link.href}
                className="rounded-md px-3 py-2 text-sm text-muted-foreground hover:text-foreground"
              >
                {link.text}
              </a>
            ))}
            <hr className="my-2 border-border" />
            <Button variant="outline" size="sm" asChild className="justify-start">
              <a href={REPO_URL} target="_blank" rel="noopener noreferrer">
                <GitHubIcon className="h-4 w-4" />
                GitHub
              </a>
            </Button>
          </nav>
        </div>
      )}
    </header>
  );
}
