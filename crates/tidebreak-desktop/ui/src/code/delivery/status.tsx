import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { CircleAlert, GitBranch, RefreshCw } from "lucide-react";
import type {
  CodeDeliveryRunSummary,
  CodeDeliverySourceError,
  CodeGitHubCapability,
} from "../../api/types";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";
import { humanize, runTone } from "./helpers";
import { relativeTime } from "../PullRequestDetail";

/**
 * How many rows there are, how old they are, and a way to reread them.
 *
 * Delivery holds live GitHub state behind a thirty-second server cache and
 * refetches only when a filter moves, so a reader watching a merge land had
 * no way to tell whether the list was current and no way to ask.
 */
export function FreshnessBar({
  fetchedAt,
  loading,
  count,
  noun,
  onRefresh,
}: {
  fetchedAt: string | null;
  loading: boolean;
  count: number;
  noun: string;
  onRefresh: () => void;
}) {
  return (
    <div className="flex items-center justify-between gap-3 px-5 py-1.5 text-xs text-muted-foreground">
      <span>
        {count === 0 ? "" : `${count} ${noun}${count === 1 ? "" : "s"}`}
        {fetchedAt && count > 0 && ` · updated ${relativeTime(fetchedAt)}`}
      </span>
      <Button
        type="button"
        size="xs"
        variant="ghost"
        disabled={loading}
        onClick={onRefresh}
      >
        {loading ? <Spinner aria-hidden /> : <RefreshCw />}
        Refresh
      </Button>
    </div>
  );
}

export function deliveryRefreshErrors(
  errors: readonly CodeDeliverySourceError[],
): CodeDeliverySourceError[] {
  return errors.filter((error) => error.kind !== "not_github");
}

function refreshErrorSummary(
  errors: readonly CodeDeliverySourceError[],
): string {
  if (errors.length === 1) return errors[0]!.message;
  const names = errors.flatMap((error) =>
    error.repository
      ? [`${error.repository.owner}/${error.repository.name}`]
      : [],
  );
  if (names.length === errors.length) {
    return `${errors.length} repositories could not be refreshed (${names.join(", ")}). Available results are still shown.`;
  }
  return `${errors.length} repositories could not be refreshed. Available results are still shown.`;
}

export function PartialErrorBanner({
  errors,
  compact = false,
}: {
  errors: CodeDeliverySourceError[];
  compact?: boolean;
}) {
  const visible = deliveryRefreshErrors(errors);
  if (visible.length === 0) return null;
  return (
    <div
      role="status"
      className={cn(
        "notice-surface notice-warning flex shrink-0 items-start gap-2 border-b px-5 py-2.5 text-xs",
        compact && "border-t",
      )}
    >
      <CircleAlert className="mt-0.5 size-3.5 shrink-0" />
      <span>{refreshErrorSummary(visible)}</span>
    </div>
  );
}

export function RepositoryRefreshWarning({
  message,
  onRetry,
}: {
  message: string;
  onRetry: () => void;
}) {
  return (
    <div className="notice-surface notice-warning flex shrink-0 items-center justify-between gap-3 border-b px-5 py-2.5 text-xs">
      <span>GitHub repository discovery is stale: {message}</span>
      <Button type="button" size="xs" variant="outline" onClick={onRetry}>
        Try again
      </Button>
    </div>
  );
}

export function GitHubUnavailable({
  capability,
}: {
  capability: CodeGitHubCapability;
}) {
  return (
    <Empty className="min-h-80">
      <EmptyHeader>
        <EmptyMedia variant="icon">
          <CircleAlert />
        </EmptyMedia>
        <EmptyTitle>GitHub is not connected</EmptyTitle>
        <EmptyDescription>{capability.remediation}</EmptyDescription>
      </EmptyHeader>
    </Empty>
  );
}

export function NoDeliveryRepositories() {
  return (
    <Empty className="min-h-80">
      <EmptyHeader>
        <EmptyMedia variant="icon">
          <GitBranch />
        </EmptyMedia>
        <EmptyTitle>No GitHub repositories tracked</EmptyTitle>
        <EmptyDescription>
          Register a GitHub-backed repo in Tidebreak or add one from
          Repositories.
        </EmptyDescription>
      </EmptyHeader>
    </Empty>
  );
}

export function InlineLoadError({
  message,
  onRetry,
}: {
  message: string;
  onRetry: () => void;
}) {
  return (
    <div className="notice-surface notice-critical m-4 flex items-center justify-between gap-3 rounded-lg border px-3 py-2 text-sm">
      <span>{message}</span>
      <Button type="button" size="xs" variant="outline" onClick={onRetry}>
        Try again
      </Button>
    </div>
  );
}

export function DeliveryListSkeleton() {
  return (
    <div
      className="flex min-h-0 w-full min-w-0 flex-1 flex-col p-5"
      role="status"
      aria-label="Loading"
    >
      <span className="sr-only">Loading</span>
      {Array.from({ length: 7 }, (_, index) => (
        <div
          key={index}
          className="grid grid-cols-[minmax(260px,1fr)_150px_120px_110px] gap-4 border-b border-border-subtle py-3"
        >
          <div className="flex flex-col gap-2">
            <Skeleton className="h-4 w-2/3" />
            <Skeleton className="h-3 w-1/2" />
          </div>
          <Skeleton className="h-5 w-24" />
          <Skeleton className="h-5 w-20" />
          <Skeleton className="ml-auto h-3 w-16" />
        </div>
      ))}
    </div>
  );
}

export function DetailStat({
  label,
  value,
  mono = false,
}: {
  label: string;
  value: string;
  mono?: boolean;
}) {
  return (
    <div className="min-w-0">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className={cn("mt-0.5 truncate", mono && "font-mono")}>{value}</dd>
    </div>
  );
}

export function RunStatusBadge({ item }: { item: CodeDeliveryRunSummary }) {
  const value = item.conclusion ?? item.status;
  const tone = runTone(value);
  return (
    <span
      className={cn(
        "rounded-md px-2 py-1 text-xs font-medium",
        tone === "success" &&
          "bg-success-background text-success-foreground-muted",
        tone === "critical" &&
          "bg-critical-background text-critical-foreground-muted",
        tone === "warning" &&
          "bg-warning-background text-warning-foreground-muted",
        tone === "muted" && "bg-muted text-muted-foreground",
      )}
    >
      {humanize(value)}
    </span>
  );
}

export function RunStateText({ value }: { value: string }) {
  const tone = runTone(value);
  return (
    <span
      className={cn(
        "text-xs font-medium",
        tone === "success" && "text-success",
        tone === "critical" && "text-critical",
        tone === "warning" && "text-warning",
        tone === "muted" && "text-muted-foreground",
      )}
    >
      {humanize(value)}
    </span>
  );
}
