import { Globe } from "lucide-react";
import { useState } from "react";

import { cn } from "./lib/utils";

type DomainFaviconProps = {
  /** Public page whose host supplies the icon. */
  url: string;
  className?: string;
};

/**
 * Site mark for a public page, keyed off its host.
 *
 * Loads from DuckDuckGo's icon service so one CSP origin covers every
 * domain. When the fetch fails — unknown host, offline, blocked — the
 * same globe every link used to show stands in, so a missing mark never
 * leaves a hole in the list.
 */
export function DomainFavicon({ url, className }: DomainFaviconProps) {
  const [failed, setFailed] = useState(false);
  const domain = hostOf(url);
  if (domain === null || failed) {
    return (
      <Globe
        className={cn("text-muted-foreground size-4 shrink-0", className)}
        aria-hidden="true"
      />
    );
  }
  return (
    <img
      src={`https://icons.duckduckgo.com/ip3/${encodeURIComponent(domain)}.ico`}
      alt=""
      className={cn("size-4 shrink-0 rounded-sm object-contain", className)}
      onError={() => setFailed(true)}
    />
  );
}

/** Bare host used as the icon key; `www.` is noise for the lookup. */
function hostOf(url: string): string | null {
  try {
    const host = new URL(url).hostname.toLowerCase();
    if (host.length === 0) return null;
    return host.replace(/^www\./, "");
  } catch {
    return null;
  }
}
