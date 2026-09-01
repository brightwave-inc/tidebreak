import { ArchiveRestore, Workflow } from "lucide-react";
import type { CodeDeliveryRunSummary } from "../../api/types";
import { RUN_GRID, RUN_ROW_HEIGHT, VirtualRows } from "./VirtualRows";
import { RunStatusBadge } from "./status";
import { cn } from "@/lib/utils";
import { relativeTime } from "../PullRequestDetail";

export function RunList({
  items,
  selectedId,
  onSelect,
  scrollRef,
}: {
  items: CodeDeliveryRunSummary[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  scrollRef: React.RefObject<HTMLDivElement | null>;
}) {
  return (
    <div
      role="list"
      aria-label="Runs and deployments"
      className="min-w-[780px]"
    >
      <div
        className={cn(
          "sticky top-0 z-10 grid gap-4 border-b border-border-subtle bg-background/95 px-5 py-2 text-xs font-medium text-muted-foreground backdrop-blur",
          RUN_GRID,
        )}
      >
        <span>Run</span>
        <span>Repository</span>
        <span>Status</span>
        <span className="text-right">Updated</span>
      </div>
      <VirtualRows
        items={items}
        scrollRef={scrollRef}
        estimateSize={RUN_ROW_HEIGHT}
      >
        {(item) => (
          <button
            type="button"
            role="listitem"
            data-active={selectedId === item.id || undefined}
            className={cn(
              "grid w-full cursor-pointer gap-4 border-b border-border-subtle px-5 py-3 text-left transition-colors hover:bg-muted/35 data-[active]:bg-muted/55",
              RUN_GRID,
            )}
            onClick={() => onSelect(item.id)}
          >
            <span className="min-w-0">
              <span className="flex min-w-0 items-center gap-2">
                {item.kind === "deployment" ? (
                  <ArchiveRestore className="size-4 shrink-0 text-muted-foreground" />
                ) : (
                  <Workflow className="size-4 shrink-0 text-muted-foreground" />
                )}
                <span className="truncate text-sm font-medium">
                  {item.name}
                </span>
              </span>
              <span className="mt-1 flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
                <span>
                  {item.kind === "deployment"
                    ? "Deployment"
                    : (item.workflow ?? "Workflow")}
                </span>
                {item.environment && <span>{item.environment}</span>}
                {item.branch && (
                  <span className="truncate font-mono">{item.branch}</span>
                )}
              </span>
            </span>
            <span className="flex min-w-0 items-center text-xs text-muted-foreground">
              <span className="truncate">
                {item.repository.name_with_owner}
              </span>
            </span>
            <span className="flex items-center">
              <RunStatusBadge item={item} />
            </span>
            <span className="flex items-center justify-end text-xs text-muted-foreground">
              {relativeTime(item.updated_at)}
            </span>
          </button>
        )}
      </VirtualRows>
    </div>
  );
}
