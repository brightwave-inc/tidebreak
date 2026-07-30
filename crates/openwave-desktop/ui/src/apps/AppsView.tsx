import { LayoutGrid, RotateCwIcon, ShieldCheck } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import type { AppSummary } from "@/api";
import { PanelSecondaryHeader } from "@/components/PanelHeader";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { WithTooltip } from "@/components/ui/tooltip";
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
    <div className="flex min-h-0 flex-1 flex-col">
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

      <div className="flex min-h-0 flex-1 flex-col gap-2 pt-4">
        {error && (
          <div
            className="mx-4 flex shrink-0 items-center justify-between gap-3 rounded-md bg-critical-background px-3 py-2 text-sm text-critical-foreground-muted"
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
          <p className="px-4 text-sm text-muted-foreground" role="status">
            Loading your apps…
          </p>
        ) : !hasApps ? (
          <Empty>
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <LayoutGrid />
              </EmptyMedia>
              <EmptyTitle>No apps yet</EmptyTitle>
              <EmptyDescription>
                Ask OpenWave to build a mini app in a conversation. What it
                creates lives here — reopen it any time without re-running the
                chat.
              </EmptyDescription>
            </EmptyHeader>
          </Empty>
        ) : (
          <ul className="flex flex-col gap-0.5 overflow-y-auto px-3" aria-label="Apps">
            {apps.map((app) => (
              <li key={app.id}>
                <button
                  type="button"
                  className="hover:bg-muted flex w-full items-center gap-2 rounded-md p-2 text-left text-sm transition-colors"
                  onClick={() => onOpen(app.id)}
                >
                  <LayoutGrid
                    className="text-muted-foreground size-4 shrink-0"
                    aria-hidden="true"
                  />
                  <span className="min-w-0 flex-1 truncate">{app.name}</span>
                  {app.granted && (
                    <Badge variant="outline" className="shrink-0 gap-1">
                      <ShieldCheck className="size-3" aria-hidden="true" />
                      Granted
                    </Badge>
                  )}
                  <span className="text-muted-foreground shrink-0 text-xs tabular-nums">
                    {revisionCountLabel(app.revision_count)} ·{" "}
                    {updatedLabel(app.updated_at)}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

function revisionCountLabel(count: number): string {
  return count === 1 ? "1 revision" : `${count} revisions`;
}

/** The row's freshness, as a plain local date; time-of-day is noise here. */
export function updatedLabel(updatedAt: string): string {
  const date = new Date(updatedAt);
  return Number.isNaN(date.getTime()) ? "" : date.toLocaleDateString();
}

export function friendlyAppsError(error: unknown, fallback: string): string {
  const message = String(error).replace(/^Error:\s*/, "").trim();
  return message && message.length <= 240 ? message : fallback;
}
