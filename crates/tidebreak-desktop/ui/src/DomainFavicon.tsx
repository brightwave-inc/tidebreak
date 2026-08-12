import { Globe } from "lucide-react";

import { cn } from "./lib/utils";

type DomainFaviconProps = {
  /** Public page represented by the local fallback. */
  url: string;
  className?: string;
};

/**
 * Privacy-preserving site mark. Rendering a source must not disclose its host
 * or the user's address to a third-party favicon service.
 */
export function DomainFavicon({ url: _url, className }: DomainFaviconProps) {
  return (
    <Globe
      className={cn("text-muted-foreground size-4 shrink-0", className)}
      aria-hidden="true"
    />
  );
}
