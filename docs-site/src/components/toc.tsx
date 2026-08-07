'use client';

import type { TocEntry } from '@/lib/content';
import { useEffect, useState } from 'react';

export function TableOfContents({ entries }: { entries: TocEntry[] }) {
  const [activeId, setActiveId] = useState('');

  useEffect(() => {
    const ids = entries.map((e) => e.id);
    const observer = new IntersectionObserver(
      (observerEntries) => {
        for (const entry of observerEntries) {
          if (entry.isIntersecting) {
            setActiveId(entry.target.id);
          }
        }
      },
      { rootMargin: '-64px 0px -80% 0px' },
    );

    for (const id of ids) {
      const el = document.getElementById(id);
      if (el) observer.observe(el);
    }

    return () => observer.disconnect();
  }, [entries]);

  if (entries.length === 0) return null;

  return (
    <aside className="hidden w-56 shrink-0 xl:block">
      <div className="sticky top-[3.75rem] py-6 pl-4">
        <p className="mb-3 text-xs font-semibold uppercase tracking-wider text-muted-foreground">
          On this page
        </p>
        <ul className="space-y-1 text-sm">
          {entries.map((entry) => (
            <li key={entry.id}>
              <a
                href={`#${entry.id}`}
                className={`block py-1 transition-colors ${
                  entry.depth > 2 ? 'pl-4' : ''
                } ${
                  activeId === entry.id
                    ? 'font-medium text-foreground'
                    : 'text-muted-foreground hover:text-foreground'
                }`}
              >
                {entry.text}
              </a>
            </li>
          ))}
        </ul>
      </div>
    </aside>
  );
}
