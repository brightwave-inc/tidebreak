import { Fragment } from "react";

import { Skeleton } from "./components/ui/skeleton";

/** Placeholder rows that echo the transcript's shape while history hydrates. */
export function TranscriptSkeleton() {
  return (
    <div className="flex w-full flex-col gap-4" aria-hidden="true">
      {[0, 1, 2, 3].map((row) => (
        <Fragment key={row}>
          <Skeleton className="h-9 w-1/2 self-start rounded-xl" />
          <div className="flex flex-col gap-2.5 py-2">
            <Skeleton className="h-3 w-full" />
            <Skeleton className="h-3 w-full" />
            <Skeleton className="h-3 w-full" />
            <Skeleton className="h-3 w-5/6" />
            <Skeleton className="h-3 w-1/3" />
          </div>
        </Fragment>
      ))}
    </div>
  );
}
