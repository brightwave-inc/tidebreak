import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";

/**
 * Loading for a panel: skeleton rows on an index, a centered spinner in a
 * viewer. Never reuse an empty-success mark while the list is still arriving.
 */
export function PanelLoading({
  variant,
  label = "Loading…",
  rows = 6,
  className,
}: {
  variant: "list" | "viewer";
  label?: string;
  rows?: number;
  className?: string;
}) {
  if (variant === "viewer") {
    return (
      <div
        className={cn("grid h-full min-h-24 place-items-center", className)}
        role="status"
        aria-label={label}
      >
        <Spinner aria-label={label} />
      </div>
    );
  }

  return (
    <div
      className={cn("flex flex-col gap-0.5 px-1 py-1", className)}
      role="status"
      aria-label={label}
    >
      {Array.from({ length: rows }, (_, index) => (
        <div
          key={index}
          className="flex h-8 items-center gap-2 rounded-md px-2.5"
        >
          <Skeleton className="size-4 shrink-0 rounded" />
          <Skeleton
            className={cn(
              "h-3",
              index % 3 === 0 ? "w-4/5" : index % 3 === 1 ? "w-3/5" : "w-2/3",
            )}
          />
        </div>
      ))}
    </div>
  );
}
