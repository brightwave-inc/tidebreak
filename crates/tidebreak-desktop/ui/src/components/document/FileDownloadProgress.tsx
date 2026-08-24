import { Loader2Icon } from "lucide-react";
import type { HTMLAttributes } from "react";

import type { FileDownloadProgress } from "@/api";
import { Progress } from "@/components/ui/progress";
import { cn } from "@/lib/utils";

/**
 * How far a large source has got, in place of a spinner that says nothing.
 *
 * Only rendered once a transfer has a declared length, so there is always a
 * total to divide by — a viewer with no progress to show keeps its spinner.
 */
export function FileDownloadProgressIndicator({
  progress,
  className,
  ...props
}: HTMLAttributes<HTMLDivElement> & { progress: FileDownloadProgress }) {
  const percentage = Math.min(100, Math.max(0, progress.percentage));

  return (
    <div
      className={cn(
        "flex h-64 items-center justify-center px-4 text-muted-foreground",
        className,
      )}
      role="status"
      aria-live="polite"
      {...props}
    >
      <div className="flex w-full max-w-64 flex-col items-center gap-3 text-center">
        <Loader2Icon className="size-6 animate-spin" />
        <div className="flex flex-col items-center gap-1">
          <p>Downloading document…</p>
          <div className="flex items-center gap-2 text-sm">
            <span className="tabular-nums">{Math.round(percentage)}%</span>
            <span className="text-muted-foreground/70 tabular-nums">
              ({megabytes(progress.loaded)} / {megabytes(progress.total)} MB)
            </span>
          </div>
        </div>
        <Progress
          value={percentage}
          aria-label="Download progress"
          className="w-full"
        />
      </div>
    </div>
  );
}

function megabytes(bytes: number): string {
  return (bytes / (1024 * 1024)).toFixed(1);
}
