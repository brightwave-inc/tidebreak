import Link from 'next/link';
import { ChevronLeft, ChevronRight } from 'lucide-react';
import { cn } from '@/lib/utils';

interface PageNavProps {
  prev: { title: string; description: string; url: string } | null;
  next: { title: string; description: string; url: string } | null;
}

export function PageNavigation({ prev, next }: PageNavProps) {
  if (!prev && !next) return null;

  return (
    <div className="mt-12 grid grid-cols-1 gap-4 sm:grid-cols-2">
      {prev ? (
        <Link
          href={prev.url}
          className="group flex flex-col rounded-lg border border-border bg-card p-4 transition-colors hover:bg-accent"
        >
          <span className="inline-flex items-center gap-1 text-sm font-semibold text-foreground">
            <ChevronLeft className="h-4 w-4 transition-transform group-hover:-translate-x-0.5" />
            {prev.title}
          </span>
          {prev.description && (
            <span className="mt-1 truncate text-sm text-muted-foreground">
              {prev.description}
            </span>
          )}
        </Link>
      ) : (
        <div />
      )}
      {next ? (
        <Link
          href={next.url}
          className={cn(
            'group flex flex-col rounded-lg border border-border bg-card p-4 text-right transition-colors hover:bg-accent',
            !prev && 'sm:col-start-2',
          )}
        >
          <span className="inline-flex items-center justify-end gap-1 text-sm font-semibold text-foreground">
            {next.title}
            <ChevronRight className="h-4 w-4 transition-transform group-hover:translate-x-0.5" />
          </span>
          {next.description && (
            <span className="mt-1 truncate text-sm text-muted-foreground">
              {next.description}
            </span>
          )}
        </Link>
      ) : (
        <div />
      )}
    </div>
  );
}
