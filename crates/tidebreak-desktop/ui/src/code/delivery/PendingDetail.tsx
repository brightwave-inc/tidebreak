import { Button } from "@/components/ui/button";
import { DetailSheet, DetailSkeleton } from "../PullRequestDetail";
import { X } from "lucide-react";

/**
 * The sheet while a route-targeted detail is still loading: the reader
 * followed a deep link, so the frame opens immediately with what the link
 * already knew — repository, number, title — and the body fills in.
 */
export function PendingDetailPane({
  context,
  title,
  closeLabel,
  onClose,
}: {
  context: string;
  title: string;
  closeLabel: string;
  onClose: () => void;
}) {
  return (
    <div
      className="flex h-full min-h-0 flex-col overflow-hidden bg-background"
      data-testid="pull-request-detail-pane"
    >
      <div className="flex shrink-0 items-start gap-3 border-b border-border-subtle px-5 py-3">
        <div className="min-w-0 flex-1">
          <div className="text-xs text-muted-foreground">{context}</div>
          <h2 className="mt-1 text-base font-semibold leading-snug">{title}</h2>
        </div>
        <Button type="button" size="icon-xs" variant="ghost" onClick={onClose}>
          <X />
          <span className="sr-only">{closeLabel}</span>
        </Button>
      </div>
      <div className="min-h-0 flex-1 overflow-auto p-5">
        <DetailSkeleton />
      </div>
    </div>
  );
}

export function PendingDetailSheet({
  context,
  title,
  closeLabel,
  onClose,
}: {
  context: string;
  title: string;
  closeLabel: string;
  onClose: () => void;
}) {
  return (
    <DetailSheet label={title} onClose={onClose}>
      <div className="flex shrink-0 items-start gap-3 border-b border-border-subtle px-5 py-3">
        <div className="min-w-0 flex-1">
          <div className="text-xs text-muted-foreground">{context}</div>
          <h2 className="mt-1 text-base font-semibold leading-snug">{title}</h2>
        </div>
        <Button type="button" size="icon-xs" variant="ghost" onClick={onClose}>
          <X />
          <span className="sr-only">{closeLabel}</span>
        </Button>
      </div>
      <div className="min-h-0 flex-1 overflow-auto p-5">
        <DetailSkeleton />
      </div>
    </DetailSheet>
  );
}
