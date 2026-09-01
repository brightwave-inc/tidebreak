import { ExternalLink, RefreshCw, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import { openInBrowser } from "@/openInBrowser";

const RELEASES_URL = "https://github.com/brightwave-inc/tidebreak/releases";

export function releaseNotesUrl(version: string | null): string {
  const normalized = version?.trim().replace(/^v/, "");
  return normalized
    ? `${RELEASES_URL}/tag/v${encodeURIComponent(normalized)}`
    : `${RELEASES_URL}/latest`;
}

export function UpdateReadyCard({
  version,
  onRestart,
  onDismiss,
}: {
  version: string | null;
  onRestart: () => void;
  onDismiss: () => void;
}) {
  return (
    <aside
      className="fixed right-4 bottom-4 z-40 w-[min(22rem,calc(100vw-2rem))] rounded-xl border border-border bg-popover p-4 text-popover-foreground shadow-lg"
      aria-label="Update ready"
    >
      <button
        type="button"
        className="absolute top-2.5 right-2.5 grid size-7 cursor-pointer place-items-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-3 focus-visible:ring-ring/25"
        aria-label="Dismiss update notice"
        onClick={onDismiss}
      >
        <X className="size-4" aria-hidden="true" />
      </button>

      <div className="flex items-start gap-3 pr-7">
        <RefreshCw
          className="mt-0.5 size-4 shrink-0 text-muted-foreground"
          aria-hidden="true"
        />
        <div className="min-w-0">
          <p className="text-md font-semibold">Update ready</p>
          <p className="mt-1 text-sm text-muted-foreground">
            {version
              ? `Tidebreak ${version} is downloaded and ready to install.`
              : "A Tidebreak update is downloaded and ready to install."}
          </p>
        </div>
      </div>

      <div className="mt-4 flex flex-wrap items-center gap-2">
        <Button type="button" size="sm" onClick={onRestart}>
          Restart and update
        </Button>
        <Button
          type="button"
          size="sm"
          variant="ghost"
          onClick={() => void openInBrowser(releaseNotesUrl(version))}
        >
          Release notes
          <ExternalLink aria-hidden="true" />
        </Button>
      </div>
    </aside>
  );
}
