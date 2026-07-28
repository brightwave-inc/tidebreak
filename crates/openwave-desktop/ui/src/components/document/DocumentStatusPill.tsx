import { InfoIcon, Loader2Icon, RotateCcwIcon } from "lucide-react";

import type { LibraryDocument } from "@/documents";
import { Badge } from "@/components/ui/badge";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";

/**
 * A source's preparation state, as a pill beside its name.
 *
 * A source that is ready and searchable draws nothing: the common case should
 * be quiet, and a column of "Ready" badges is noise the reader learns to skip.
 * Only the states that mean something — still working, readable but not
 * searchable, failed — get a pill, and the two that need explaining carry it
 * in a popover rather than spilling a sentence into the row.
 */
export function DocumentStatusPill({
  document,
  isRetryPending,
  onRetryClick,
}: {
  document: LibraryDocument;
  isRetryPending?: boolean;
  onRetryClick?: () => void;
}) {
  switch (document.processingStatus) {
    case "queued":
    case "processing":
      return (
        <Badge size="sm" variant="secondary" className="animate-pulse" role="status">
          Preparing
        </Badge>
      );

    case "ready":
      if (document.searchable) return null;
      return (
        <ExplainedBadge
          variant="warning"
          label="Not searchable"
          message="OpenWave stored this file, but found no searchable text in it. It is still available to the conversation as a source."
        />
      );

    case "failed": {
      const failure = document.failure;
      const badge = (
        <ExplainedBadge
          variant="destructive"
          label="Failed"
          message={failure?.message ?? "OpenWave could not prepare this source."}
        />
      );
      if (!failure?.retriable || !onRetryClick) return badge;
      return (
        <div className="flex items-center gap-1.5">
          {badge}
          <Badge
            size="sm"
            variant="secondary"
            role="button"
            tabIndex={0}
            aria-label={isRetryPending ? "Retrying" : "Retry"}
            className="cursor-pointer"
            onClick={onRetryClick}
            onKeyDown={(event) => {
              if (event.key === "Enter" || event.key === " ") {
                event.preventDefault();
                onRetryClick();
              }
            }}
          >
            {isRetryPending ? (
              <Loader2Icon className="mr-0.5 size-3 animate-spin" />
            ) : (
              <RotateCcwIcon className="mr-0.5 size-3" />
            )}
            {isRetryPending ? "Retrying…" : "Retry"}
          </Badge>
        </div>
      );
    }
  }
}

function ExplainedBadge({
  variant,
  label,
  message,
}: {
  variant: "warning" | "destructive";
  label: string;
  message: string;
}) {
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Badge size="sm" variant={variant} role="button" tabIndex={0} className="cursor-pointer">
          {label}
        </Badge>
      </PopoverTrigger>
      <PopoverContent className="w-auto max-w-xs p-3 text-sm">
        <div className="flex items-start gap-1.5">
          <InfoIcon className="mt-0.5 size-3 shrink-0 text-muted-foreground" />
          <span>{message}</span>
        </div>
      </PopoverContent>
    </Popover>
  );
}
