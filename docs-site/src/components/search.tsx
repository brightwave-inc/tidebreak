'use client';

import { useEffect, useRef, useState, useCallback } from 'react';
import { create, insert, search as oramaSearch, type AnyOrama } from '@orama/orama';
import { Search, FileText } from 'lucide-react';
import { useRouter } from 'next/navigation';
import {
  Dialog,
  DialogContent,
  DialogTitle,
} from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import * as VisuallyHidden from '@radix-ui/react-visually-hidden';

interface SearchEntry {
  title: string;
  description: string;
  url: string;
  content: string;
}

interface SearchResult {
  title: string;
  description: string;
  url: string;
}

export function SearchDialog() {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [results, setResults] = useState<SearchResult[]>([]);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const dbRef = useRef<AnyOrama | null>(null);
  const router = useRouter();

  useEffect(() => {
    function onOpenSearch() {
      setOpen(true);
    }
    window.addEventListener('open-search', onOpenSearch);
    return () => window.removeEventListener('open-search', onOpenSearch);
  }, []);

  const doSearch = useCallback(async (q: string) => {
    if (!dbRef.current || !q.trim()) {
      setResults([]);
      return;
    }

    const res = await oramaSearch(dbRef.current, {
      term: q,
      limit: 10,
    });

    setResults(
      res.hits.map((hit) => {
        const doc = hit.document as unknown as SearchEntry;
        return {
          title: doc.title,
          description: doc.description,
          url: doc.url,
        };
      }),
    );
    setSelectedIndex(0);
  }, []);

  useEffect(() => {
    if (!open) return;

    if (dbRef.current) return;

    const basePath = process.env.__NEXT_ROUTER_BASEPATH || '';
    fetch(`${basePath}/search-index.json`)
      .then((r) => (r.ok ? r.json() : []))
      .then(async (entries: SearchEntry[]) => {
        const db = await create({
          schema: {
            title: 'string',
            description: 'string',
            url: 'string',
            content: 'string',
          },
          language: 'english',
        });

        for (const entry of entries) {
          await insert(db, entry);
        }

        dbRef.current = db;

        if (query) {
          await doSearch(query);
        }
      })
      // No index yet (content still being authored) just means no results.
      .catch(() => {});
  // eslint-disable-next-line react-hooks/exhaustive-deps -- query is read at fetch-resolve time, not as a reactive dep
  }, [open, doSearch]);

  function handleQueryChange(q: string) {
    setQuery(q);
    doSearch(q);
  }

  function navigate(url: string) {
    setOpen(false);
    setQuery('');
    router.push(url);
  }

  function onKeyDown(e: React.KeyboardEvent) {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      setSelectedIndex((i) => Math.min(i + 1, results.length - 1));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setSelectedIndex((i) => Math.max(i - 1, 0));
    } else if (e.key === 'Enter' && results[selectedIndex]) {
      navigate(results[selectedIndex].url);
    }
  }

  return (
    <Dialog open={open} onOpenChange={(v) => { setOpen(v); if (!v) setQuery(''); }}>
      <DialogContent className="top-[20%] translate-y-0 gap-0 p-0 sm:max-w-lg">
        <VisuallyHidden.Root>
          <DialogTitle>Search documentation</DialogTitle>
        </VisuallyHidden.Root>

        <div className="flex items-center gap-3 border-b border-border px-4">
          <Search className="h-4 w-4 shrink-0 text-muted-foreground" />
          <Input
            value={query}
            onChange={(e) => handleQueryChange(e.target.value)}
            onKeyDown={onKeyDown}
            placeholder="Search documentation…"
            className="h-12 border-0 bg-transparent px-0 shadow-none focus-visible:ring-0 focus-visible:ring-offset-0"
          />
        </div>

        {query.trim() && (
          <div className="max-h-80 overflow-y-auto p-2">
            {results.length === 0 ? (
              <p className="px-3 py-6 text-center text-sm text-muted-foreground">
                No results found.
              </p>
            ) : (
              <ul>
                {results.map((result, i) => (
                  <li key={result.url}>
                    <button
                      onClick={() => navigate(result.url)}
                      onMouseEnter={() => setSelectedIndex(i)}
                      className={`flex w-full items-start gap-3 rounded-md px-3 py-2.5 text-left transition-colors ${
                        i === selectedIndex ? 'bg-accent' : ''
                      }`}
                    >
                      <FileText className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" />
                      <div className="min-w-0">
                        <p className="text-sm font-medium text-foreground">
                          {result.title}
                        </p>
                        {result.description && (
                          <p className="mt-0.5 truncate text-xs text-muted-foreground">
                            {result.description}
                          </p>
                        )}
                      </div>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}

        {!query.trim() && (
          <div className="p-6 text-center text-sm text-muted-foreground">
            Start typing to search…
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
