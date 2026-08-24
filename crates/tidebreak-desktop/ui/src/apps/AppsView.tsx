import {
  CircleAlert,
  LayoutGrid,
  RotateCwIcon,
  ShieldCheck,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";

import type { AppSummary } from "@/api";
import { PanelSecondaryHeader } from "@/components/PanelHeader";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { Skeleton } from "@/components/ui/skeleton";
import { WithTooltip } from "@/components/ui/tooltip";
import { friendlyErrorMessage } from "@/lib/utils";
import type { AppsApis } from "./appsApis";

/**
 * The Apps library, as the panel addressed `apps`.
 *
 * Profile-scoped rather than conversation-scoped: an app outlives every chat
 * that touched it, which is why this list hangs off the home rail instead of
 * a conversation's. Picking a row opens `apps.{appId}` — the detail panel
 * with the running app, its consent state, and its history.
 */
export function AppsView({
  onOpen,
  apis,
}: {
  /** Navigate to the `apps.{appId}` panel contract. */
  onOpen: (appId: string) => void;
  apis: AppsApis;
}) {
  const [apps, setApps] = useState<AppSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const generationRef = useRef(0);

  async function refresh(showLoading = false) {
    const generation = ++generationRef.current;
    if (showLoading) setLoading(true);
    setError(null);
    try {
      const library = await apis.list();
      if (generation !== generationRef.current) return;
      setApps(library.apps);
    } catch (caught) {
      if (generation !== generationRef.current) return;
      setError(friendlyAppsError(caught, "Could not load your apps."));
    } finally {
      if (generation === generationRef.current) setLoading(false);
    }
  }

  useEffect(() => {
    void refresh(true);
    return () => {
      generationRef.current += 1;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [apis]);

  const hasApps = apps.length > 0;

  return (
    <div className="flex h-full min-h-0 flex-1 flex-col overflow-hidden">
      <PanelSecondaryHeader showBorder={false} className="pr-1 pl-4">
        <h1 className="text-lg font-medium">Apps</h1>
        <span className="grow" />
        <div className="pr-2">
          <WithTooltip label="Refresh">
            <Button
              variant="ghost"
              size="icon-sm"
              disabled={loading}
              onClick={() => void refresh(true)}
            >
              <RotateCwIcon className="size-4" />
              <span className="sr-only">Refresh</span>
            </Button>
          </WithTooltip>
        </div>
      </PanelSecondaryHeader>

      <div className="min-h-0 flex-1 overflow-y-auto px-3 pt-4 pb-6">
        <div className="mx-auto flex w-full max-w-3xl flex-col gap-3">
          {error && hasApps && (
            <div
              className="flex shrink-0 items-center justify-between gap-3 rounded-lg bg-critical-background px-3 py-2 text-sm text-critical-foreground-muted"
              role="alert"
            >
              <span>{error}</span>
              <Button
                variant="outline"
                size="xs"
                className="shrink-0"
                onClick={() => void refresh(true)}
              >
                Try again
              </Button>
            </div>
          )}

          {loading && !hasApps ? (
            <AppsLoading />
          ) : error && !hasApps ? (
            <AppsFailure error={error} onRetry={() => void refresh(true)} />
          ) : !hasApps ? (
            <Empty className="min-h-80 border">
              <EmptyHeader>
                <EmptyMedia variant="icon" className="text-icon-blue">
                  <LayoutGrid />
                </EmptyMedia>
                <EmptyTitle>No apps yet</EmptyTitle>
                <EmptyDescription>
                  Ask Tidebreak to build a mini app in a conversation. Each app
                  stays here so you can open it again without re-running the
                  work.
                </EmptyDescription>
              </EmptyHeader>
            </Empty>
          ) : (
            <section
              aria-labelledby="saved-apps-title"
              className="flex flex-col gap-2"
            >
              <div className="flex items-end justify-between gap-4 px-1">
                <div>
                  <h2 id="saved-apps-title" className="text-sm font-medium">
                    Saved apps
                  </h2>
                  <p className="text-xs text-muted-foreground">
                    Mini apps built in your conversations.
                  </p>
                </div>
                <span className="shrink-0 text-xs tabular-nums text-muted-foreground">
                  {apps.length}
                </span>
              </div>
              <ul
                className="divide-y divide-border-subtle overflow-hidden rounded-xl border bg-background"
                aria-label="Apps"
              >
                {apps.map((app) => (
                  <li key={app.id}>
                    <button
                      type="button"
                      className="group flex w-full items-center gap-3 px-3 py-3 text-left transition-colors hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
                      onClick={() => onOpen(app.id)}
                    >
                      <span className="grid size-9 shrink-0 place-items-center rounded-lg border border-border-subtle bg-muted/35">
                        <LayoutGrid
                          className="size-4 text-icon-blue"
                          aria-hidden="true"
                        />
                      </span>
                      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                        <span className="truncate text-sm font-medium">
                          {app.name}
                        </span>
                        <span className="truncate text-xs tabular-nums text-muted-foreground">
                          {revisionCountLabel(app.revision_count)} · Updated{" "}
                          {updatedLabel(app.updated_at)}
                        </span>
                      </span>
                      {app.granted && (
                        <Badge variant="success" size="sm" className="shrink-0">
                          <ShieldCheck className="size-3" aria-hidden="true" />
                          Access allowed
                        </Badge>
                      )}
                    </button>
                  </li>
                ))}
              </ul>
            </section>
          )}
        </div>
      </div>
    </div>
  );
}

function AppsFailure({
  error,
  onRetry,
}: {
  error: string;
  onRetry: () => void;
}) {
  return (
    <Empty className="min-h-80 border" role="alert">
      <EmptyHeader>
        <EmptyMedia variant="icon" className="text-critical">
          <CircleAlert />
        </EmptyMedia>
        <EmptyTitle>Apps could not load</EmptyTitle>
        <EmptyDescription>{error}</EmptyDescription>
      </EmptyHeader>
      <EmptyContent>
        <Button variant="outline" size="sm" onClick={onRetry}>
          Try again
        </Button>
      </EmptyContent>
    </Empty>
  );
}

function AppsLoading() {
  return (
    <div
      className="overflow-hidden rounded-xl border bg-background"
      role="status"
      aria-label="Loading your apps"
    >
      <span className="sr-only">Loading your apps…</span>
      {["first", "second", "third"].map((row) => (
        <div
          key={row}
          className="flex items-center gap-3 border-b border-border-subtle px-3 py-3 last:border-b-0"
        >
          <Skeleton className="size-9 shrink-0" />
          <div className="flex min-w-0 flex-1 flex-col gap-2">
            <Skeleton className="h-3 w-40 max-w-full" />
            <Skeleton className="h-2.5 w-28 max-w-3/4" />
          </div>
          <Skeleton className="h-5 w-20 shrink-0 rounded-full" />
        </div>
      ))}
    </div>
  );
}

function revisionCountLabel(count: number): string {
  return count === 1 ? "1 revision" : `${count} revisions`;
}

/** The row's freshness, as a plain local date; time-of-day is noise here. */
export function updatedLabel(updatedAt: string): string {
  const date = new Date(updatedAt);
  return Number.isNaN(date.getTime())
    ? ""
    : date.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

export function friendlyAppsError(error: unknown, fallback: string): string {
  return friendlyErrorMessage(error, fallback);
}
