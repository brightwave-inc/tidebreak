import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ComponentType,
} from "react";
import { Gauge, RefreshCw } from "lucide-react";

import { useApp } from "@/AppContext";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/utils";
import { ClaudeIcon, OpenAIIcon, XaiIcon } from "@/ProviderIcons";
import { SidebarButton } from "@/sidebar/primitives";
import type {
  CodeSubscriptionUsage,
  CodeSubscriptionUsageAccount,
  CodeSubscriptionUsageProvider,
  CodeSubscriptionUsageWindow,
} from "../api/types";
import { FOCUS_RING } from "./interactive";

const REFRESH_INTERVAL_MS = 60_000;

/**
 * The code rail's subscription-quota surface.
 *
 * The closed state is deliberately one quiet status row. The open state has
 * enough room for every provider window, but keeps shared team subscriptions
 * behind progressive disclosure so a large Model Gateway roster does not bury
 * the reader's own accounts.
 */
export function CodeSubscriptionUsage() {
  const { client } = useApp();
  const [report, setReport] = useState<CodeSubscriptionUsage | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showShared, setShowShared] = useState(false);
  const refreshInFlight = useRef(false);

  const refresh = useCallback(async () => {
    if (refreshInFlight.current) return;
    refreshInFlight.current = true;
    setRefreshing(true);
    try {
      const next = await client.getCodeSubscriptionUsage();
      setReport(next);
      setError(null);
    } catch {
      setError("Usage could not be refreshed.");
    } finally {
      refreshInFlight.current = false;
      setRefreshing(false);
    }
  }, [client]);

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), REFRESH_INTERVAL_MS);
    return () => window.clearInterval(timer);
  }, [refresh]);

  const summary = useMemo(() => usageSummary(report), [report]);

  return (
    <Popover>
      <PopoverTrigger asChild>
        <SidebarButton
          aria-label={summary.ariaLabel}
          className="group/usage"
        >
          <Gauge className={summary.toneClass} />
          <span className="min-w-0 flex-1 truncate">Usage</span>
          {summary.percent !== null ? (
            <span className="flex shrink-0 items-center gap-1.5">
              <span className="bg-muted h-1.5 w-11 overflow-hidden rounded-full">
                <span
                  className={cn("block h-full rounded-full", summary.barClass)}
                  style={{ width: `${Math.min(100, summary.percent)}%` }}
                />
              </span>
              <span className={cn("w-8 text-right text-xs tabular-nums", summary.toneClass)}>
                {Math.round(summary.percent)}%
              </span>
            </span>
          ) : (
            <span className="text-muted-foreground text-xs">
              {refreshing ? "Loading" : "Unavailable"}
            </span>
          )}
        </SidebarButton>
      </PopoverTrigger>
      <PopoverContent
        side="right"
        align="end"
        sideOffset={8}
        className="flex max-h-[min(560px,calc(100vh-24px))] w-[min(390px,calc(100vw-24px))] flex-col gap-0 overflow-hidden p-0"
      >
        <div className="flex shrink-0 items-center justify-between gap-3 border-b px-3 py-2.5">
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <h2 className="truncate text-[13px] font-semibold">Subscription usage</h2>
              {report && (
                <span className="text-muted-foreground shrink-0 text-[10px] font-medium">
                  {sourceLabel(report.source)}
                </span>
              )}
            </div>
          </div>
          <button
            type="button"
            className={cn(
              "text-muted-foreground hover:bg-muted hover:text-foreground cursor-pointer rounded-md p-1",
              FOCUS_RING,
            )}
            aria-label="Refresh subscription usage"
            disabled={refreshing}
            onClick={() => void refresh()}
          >
            <RefreshCw className={cn("size-3.5", refreshing && "animate-spin")} />
          </button>
        </div>

        <div className="min-h-0 overflow-y-auto px-3 py-2.5">
          {error && <p className="text-critical text-xs">{error}</p>}
          {!report && !error && <UsageSkeleton />}
          {report && report.providers.length === 0 && (
            <UnavailableUsage diagnostics={report.diagnostics} />
          )}
          {report && report.providers.length > 0 && (
            <div className="flex flex-col gap-3">
              {report.diagnostics.length > 0 && (
                <p className="text-muted-foreground text-[11px]">
                  {report.diagnostics[0]}
                </p>
              )}
              {report.providers.map((provider) => (
                <ProviderUsage
                  key={provider.id}
                  provider={provider}
                  showShared={showShared}
                />
              ))}
              {hasSharedAccounts(report) && (
                <button
                  type="button"
                  className={cn(
                    "text-muted-foreground hover:text-foreground w-fit cursor-pointer text-xs underline-offset-2 hover:underline",
                    FOCUS_RING,
                  )}
                  onClick={() => setShowShared((shown) => !shown)}
                >
                  {showShared ? "Hide shared accounts" : "Show shared accounts"}
                </button>
              )}
            </div>
          )}
        </div>
      </PopoverContent>
    </Popover>
  );
}

function ProviderUsage({
  provider,
  showShared,
}: {
  provider: CodeSubscriptionUsageProvider;
  showShared: boolean;
}) {
  const Icon = providerIcon(provider.id);
  const own = provider.accounts.filter((account) => account.is_own);
  const shown = showShared
    ? provider.accounts
    : own.length > 0
      ? own
      : provider.accounts.slice(0, 1);
  return (
    <section className="flex flex-col gap-1.5">
      <div className="flex min-h-6 items-center gap-1.5">
        <span className="border-border/70 bg-muted/50 grid size-6 place-items-center rounded border">
          <Icon className="size-3" />
        </span>
        <div className="flex min-w-0 flex-1 items-baseline justify-between gap-2">
          <h3 className="truncate text-xs font-semibold">{provider.label}</h3>
          {provider.accounts.length > shown.length && (
            <span className="text-muted-foreground shrink-0 text-[9px]">
              {provider.accounts.length - shown.length} shared hidden
            </span>
          )}
        </div>
      </div>
      <div className="flex flex-col pl-[30px]">
        {shown.map((account, index) => (
          <AccountUsage key={account.id} account={account} separated={index > 0} />
        ))}
      </div>
    </section>
  );
}

function AccountUsage({
  account,
  separated,
}: {
  account: CodeSubscriptionUsageAccount;
  separated: boolean;
}) {
  return (
    <div
      className={cn(
        "flex flex-col gap-1.5 py-1",
        separated && "border-border/60 mt-1 border-t pt-2",
      )}
    >
      <div className="flex min-w-0 items-baseline justify-between gap-2">
        <span className="min-w-0 truncate text-[11px] font-medium">{account.label}</span>
        <span className="text-muted-foreground flex shrink-0 items-center gap-1.5 text-[9px] tabular-nums">
          {account.state !== "available" && (
            <span className={accountStateClass(account.state)}>
              {accountStateLabel(account.state)}
            </span>
          )}
          {account.updated_at_unix_seconds && (
            <span title={`Updated ${formatAge(account.updated_at_unix_seconds)}`}>
              {formatAge(account.updated_at_unix_seconds)}
            </span>
          )}
        </span>
      </div>
      <div className="flex flex-col gap-1">
        {account.windows.map((window) => (
          <UsageWindowRow key={`${account.id}-${window.key}`} window={window} />
        ))}
      </div>
    </div>
  );
}

function UsageWindowRow({ window }: { window: CodeSubscriptionUsageWindow }) {
  const percent = Math.max(0, window.used_percent);
  const tone = usageTone(percent, window.status);
  const reset = window.resets_at_unix_seconds
    ? formatReset(window.resets_at_unix_seconds)
    : null;
  return (
    <div className="grid min-h-5 grid-cols-[minmax(72px,0.9fr)_minmax(52px,1.1fr)_auto_auto] items-center gap-x-2 text-[10px]">
      <span className="text-muted-foreground min-w-0 truncate" title={window.label}>
        {window.label}
      </span>
      <div
        className="bg-muted h-1 overflow-hidden rounded-full"
        role="progressbar"
        aria-label={`${window.label} usage`}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.min(100, percent)}
        aria-valuetext={formatPercent(percent)}
      >
        <div
          className={cn("h-full rounded-full transition-[width] duration-300", tone.bar)}
          style={{ width: `${Math.min(100, percent)}%` }}
        />
      </div>
      <span
        className={cn("w-8 shrink-0 text-right font-medium tabular-nums", tone.text)}
        title={formatPercent(percent)}
      >
        {formatPercentCompact(percent)}
      </span>
      <span
        className="text-muted-foreground w-11 shrink-0 text-right tabular-nums"
        title={reset ?? "Reset time unavailable"}
      >
        {window.resets_at_unix_seconds
          ? formatResetCompact(window.resets_at_unix_seconds)
          : "—"}
      </span>
    </div>
  );
}

function UsageSkeleton() {
  return (
    <div className="flex animate-pulse flex-col gap-4" role="status" aria-label="Loading usage">
      {[0, 1].map((index) => (
        <div key={index} className="flex gap-3">
          <div className="bg-muted size-7 shrink-0 rounded-md" />
          <div className="flex flex-1 flex-col gap-2">
            <div className="bg-muted h-3 w-24 rounded" />
            <div className="bg-muted h-1.5 w-full rounded-full" />
          </div>
        </div>
      ))}
    </div>
  );
}

function UnavailableUsage({ diagnostics }: { diagnostics: string[] }) {
  return (
    <div className="flex flex-col gap-2 py-2">
      <p className="text-sm font-medium">No machine-readable limits found</p>
      <p className="text-muted-foreground text-xs leading-5">
        Tidebreak reads Model Gateway when modelctl is available, then falls
        back to Codex directly. Claude Code and Grok do not currently expose a
        stable non-interactive subscription-usage command.
      </p>
      {diagnostics[0] && (
        <p className="text-muted-foreground text-[11px]">{diagnostics[0]}</p>
      )}
    </div>
  );
}

function usageSummary(report: CodeSubscriptionUsage | null): {
  percent: number | null;
  ariaLabel: string;
  toneClass: string;
  barClass: string;
} {
  const windows = report?.providers.flatMap((provider) => {
    const own = provider.accounts.filter((account) => account.is_own);
    const accounts = own.length > 0 ? own : provider.accounts;
    return accounts.flatMap((account) => account.windows);
  });
  const mostUsed = windows?.reduce<CodeSubscriptionUsageWindow | null>(
    (current, window) =>
      current === null || window.used_percent > current.used_percent
        ? window
        : current,
    null,
  );
  if (!mostUsed) {
    return {
      percent: null,
      ariaLabel: "Subscription usage unavailable",
      toneClass: "text-muted-foreground",
      barClass: "bg-muted-foreground",
    };
  }
  const tone = usageTone(mostUsed.used_percent, mostUsed.status);
  return {
    percent: mostUsed.used_percent,
    ariaLabel: `Subscription usage, highest window ${formatPercent(mostUsed.used_percent)}`,
    toneClass: tone.text,
    barClass: tone.bar,
  };
}

function usageTone(percent: number, status?: string) {
  if (percent >= 100 || status === "rejected") {
    return { text: "text-critical", bar: "bg-critical" };
  }
  if (percent >= 85 || status === "allowed_warning") {
    return { text: "text-warning-foreground", bar: "bg-warning" };
  }
  return { text: "text-foreground", bar: "bg-foreground/55" };
}

function providerIcon(id: string): ComponentType<{ className?: string }> {
  switch (id) {
    case "anthropic":
      return ClaudeIcon;
    case "openai":
      return OpenAIIcon;
    case "xai":
      return XaiIcon;
    default:
      return Gauge;
  }
}

function sourceLabel(source: CodeSubscriptionUsage["source"]): string {
  switch (source) {
    case "model_gateway":
      return "Model Gateway";
    case "direct":
      return "Direct CLI";
    case "unavailable":
      return "Unavailable";
  }
}

function hasSharedAccounts(report: CodeSubscriptionUsage): boolean {
  return report.providers.some((provider) =>
    provider.accounts.some((account) => !account.is_own),
  );
}

function accountStateLabel(state: string): string {
  if (state === "available") return "Available";
  if (state === "cooling_down" || state === "limited") return "Limited";
  if (state === "reauthorization_required") return "Sign in again";
  return state.replaceAll("_", " ");
}

function accountStateClass(state: string): string {
  if (state === "available") return "text-muted-foreground";
  if (state === "cooling_down" || state === "limited") return "text-critical";
  return "text-warning-foreground";
}

function formatPercent(percent: number): string {
  const rounded = Math.round(percent * 10) / 10;
  return `${rounded}% used`;
}

function formatPercentCompact(percent: number): string {
  const rounded = Math.round(percent * 10) / 10;
  return `${rounded}%`;
}

function formatReset(timestamp: number): string {
  const seconds = timestamp - Date.now() / 1000;
  if (seconds < -30) return "Reset time passed";
  if (seconds <= 30) return "Resets now";
  return `Resets in ${formatDuration(seconds)}`;
}

function formatResetCompact(timestamp: number): string {
  const seconds = timestamp - Date.now() / 1000;
  if (seconds < -30) return "passed";
  if (seconds <= 30) return "now";
  return formatDuration(seconds);
}

function formatAge(timestamp: number): string {
  const seconds = Math.max(0, Date.now() / 1000 - timestamp);
  if (seconds < 60) return "just now";
  return `${formatDuration(seconds)} ago`;
}

function formatDuration(totalSeconds: number): string {
  const minutes = Math.max(1, Math.round(totalSeconds / 60));
  const days = Math.floor(minutes / 1_440);
  const hours = Math.floor((minutes % 1_440) / 60);
  const remainingMinutes = minutes % 60;
  if (days > 0) return hours > 0 ? `${days}d ${hours}h` : `${days}d`;
  if (hours > 0) return remainingMinutes > 0 ? `${hours}h ${remainingMinutes}m` : `${hours}h`;
  return `${remainingMinutes}m`;
}
