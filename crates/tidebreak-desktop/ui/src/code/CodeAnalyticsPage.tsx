import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Activity,
  ChartNoAxesCombined,
  CircleAlert,
  CircleDollarSign,
  Cpu,
  Database,
  Gauge,
  GitPullRequest,
  RefreshCw,
} from "lucide-react";

import { useApp } from "@/AppContext";
import type {
  CodeAnalyticsDay,
  CodeAnalyticsRange,
  CodeAnalyticsSnapshot,
  CodeSubscriptionUsage,
  HarnessKind,
} from "@/api/types";
import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { RouteFrame } from "@/RouteFrame";
import { CodeSidebar } from "./CodeSidebar";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeSubscriptionUsage } from "./useCodeSubscriptionUsage";

type TrendMetric = "tokens" | "turns" | "cost";

const RANGE_OPTIONS: readonly {
  value: CodeAnalyticsRange;
  label: string;
}[] = [
  { value: "7d", label: "7 days" },
  { value: "30d", label: "30 days" },
  { value: "90d", label: "90 days" },
  { value: "all", label: "All time" },
];

export function CodeAnalyticsPage() {
  return (
    <RouteFrame sidebar={<CodeSidebar />}>
      <div className="content-container min-h-0 w-full min-w-0 flex-1 overflow-hidden">
        <CodeAnalyticsBody />
      </div>
    </RouteFrame>
  );
}

export function CodeAnalyticsBody() {
  const { client } = useApp();
  const repos = useCodeCatalogStore((state) => state.repos);
  const catalogRefresh = useCodeCatalogStore((state) => state.refresh);
  const [range, setRange] = useState<CodeAnalyticsRange>("30d");
  const [repoId, setRepoId] = useState("all");
  const [report, setReport] = useState<CodeAnalyticsSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const requestSequence = useRef(0);
  const quota = useCodeSubscriptionUsage();

  useEffect(() => {
    void catalogRefresh(client);
  }, [catalogRefresh, client]);

  const refresh = useCallback(async () => {
    const request = ++requestSequence.current;
    setRefreshing(true);
    try {
      const next = await client.getCodeAnalytics(
        range,
        repoId === "all" ? undefined : repoId,
      );
      if (request !== requestSequence.current) return;
      setReport(next);
      setError(null);
    } catch (caught) {
      if (request !== requestSequence.current) return;
      setError(friendlyErrorMessage(caught, "Could not load code analytics."));
    } finally {
      if (request === requestSequence.current) {
        setLoading(false);
        setRefreshing(false);
      }
    }
  }, [client, range, repoId]);

  useEffect(() => {
    setLoading(true);
    setReport(null);
    setError(null);
    void refresh();
    return () => {
      requestSequence.current += 1;
    };
  }, [refresh]);

  return (
    <div className="flex size-full min-h-0 flex-col bg-background">
      <header className="shrink-0 border-b border-border-subtle px-5 py-4">
        <div className="flex flex-wrap items-start justify-between gap-4">
          <div>
            <div className="flex items-center gap-2">
              <h1 className="text-xl font-semibold tracking-tight">
                Analytics
              </h1>
              {report && !loading && (
                <span className="text-xs text-muted-foreground">
                  {rangeLabel(report.range)}
                </span>
              )}
            </div>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <Select value={repoId} onValueChange={setRepoId}>
              <SelectTrigger size="sm" className="w-44">
                <SelectValue placeholder="All repositories" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All repositories</SelectItem>
                {repos.map((repo) => (
                  <SelectItem key={repo.id} value={repo.id}>
                    {repo.display_name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <RangePicker value={range} onChange={setRange} />
            <Button
              type="button"
              size="sm"
              variant="outline"
              disabled={refreshing}
              onClick={() => void refresh()}
            >
              <RefreshCw className={cn(refreshing && "animate-spin")} />
              Refresh
            </Button>
          </div>
        </div>
      </header>

      <main className="min-h-0 flex-1 overflow-y-auto bg-page-background px-5 py-5">
        {loading && !report ? (
          <AnalyticsSkeleton />
        ) : error && !report ? (
          <AnalyticsError message={error} onRetry={() => void refresh()} />
        ) : report && report.totals.turns === 0 ? (
          <AnalyticsEmpty quota={quota.report} />
        ) : report ? (
          <>
            {error && (
              <AnalyticsRefreshError
                message={error}
                onRetry={() => void refresh()}
              />
            )}
            <AnalyticsDashboard report={report} quota={quota} />
          </>
        ) : null}
      </main>
    </div>
  );
}

function AnalyticsDashboard({
  report,
  quota,
}: {
  report: CodeAnalyticsSnapshot;
  quota: ReturnType<typeof useCodeSubscriptionUsage>;
}) {
  const totals = report.totals;
  const terminalTurns =
    totals.completed_turns + totals.failed_turns + totals.interrupted_turns;
  const completionRate =
    terminalTurns === 0 ? null : totals.completed_turns / terminalTurns;
  const failureRate =
    terminalTurns === 0
      ? null
      : (totals.failed_turns + totals.interrupted_turns) / terminalTurns;
  const coverage = pricingCoverage(report);
  const uniqueModels = new Set(
    report.models.map((model) => model.model_id ?? "unspecified"),
  ).size;
  const hasPricedUsage = report.pricing.priced_tokens > 0;

  return (
    <div className="mx-auto flex w-full max-w-[1500px] flex-col gap-4">
      <section
        className="grid gap-3 sm:grid-cols-2 xl:grid-cols-6"
        aria-label="Analytics totals"
      >
        <MetricCard
          icon={Activity}
          label="Sessions"
          value={formatNumber(totals.sessions)}
          detail={`${formatNumber(totals.turns)} turns`}
        />
        <MetricCard
          icon={ChartNoAxesCombined}
          label="Completion"
          value={formatPercent(completionRate)}
          detail={
            failureRate === null
              ? "No finished turns"
              : `${formatPercent(failureRate)} failed or stopped`
          }
        />
        <MetricCard
          icon={Database}
          label="Tokens"
          value={formatCompact(totals.total_tokens)}
          detail={`${formatCompact(totals.cache_read_tokens)} cache reads`}
        />
        <MetricCard
          icon={CircleDollarSign}
          label="Estimated cost"
          value={
            hasPricedUsage
              ? formatUsdMicros(totals.estimated_cost_microusd)
              : "—"
          }
          detail={
            !hasPricedUsage || coverage === null
              ? "No priced usage"
              : `${formatPercent(coverage)} of tokens priced`
          }
        />
        <MetricCard
          icon={GitPullRequest}
          label="Pull requests"
          value={formatNumber(totals.pull_requests_opened)}
          detail={`${formatNumber(totals.pull_requests_merged)} merged`}
        />
        <MetricCard
          icon={Cpu}
          label="Models"
          value={formatNumber(uniqueModels)}
          detail={`${formatNumber(report.harnesses.length)} harnesses`}
        />
      </section>

      <section className="grid min-w-0 gap-4 xl:grid-cols-[minmax(0,1.65fr)_minmax(280px,0.75fr)]">
        <TrendCard days={report.daily} />
        <QuotaCard quota={quota} />
      </section>

      <section className="grid min-w-0 gap-4 xl:grid-cols-[minmax(0,1.2fr)_minmax(0,0.8fr)]">
        <RepositoryCard report={report} />
        <ModelCard report={report} />
      </section>

      <section className="grid min-w-0 gap-4 lg:grid-cols-2">
        <TokenMixCard report={report} />
        <OutcomeCard report={report} />
      </section>
    </div>
  );
}

function MetricCard({
  icon: Icon,
  label,
  value,
  detail,
}: {
  icon: typeof Activity;
  label: string;
  value: string;
  detail: string;
}) {
  return (
    <article className="min-w-0 rounded-xl border border-border bg-background p-3.5">
      <div className="flex items-center justify-between gap-3">
        <span className="text-xs font-medium text-muted-foreground">
          {label}
        </span>
        <Icon className="size-3.5 text-muted-foreground" aria-hidden="true" />
      </div>
      <p className="mt-2 truncate text-2xl font-semibold tracking-tight tabular-nums">
        {value}
      </p>
      <p className="mt-1 truncate text-xs text-muted-foreground">{detail}</p>
    </article>
  );
}

function TrendCard({ days }: { days: CodeAnalyticsDay[] }) {
  const [metric, setMetric] = useState<TrendMetric>("tokens");
  const values = days.map((day) => trendValue(day, metric));
  const total = values.reduce((sum, value) => sum + value, 0);
  return (
    <section className="min-w-0 rounded-xl border border-border bg-background">
      <div className="flex flex-wrap items-start justify-between gap-3 border-b border-border-subtle px-4 py-3.5">
        <div>
          <h2 className="text-md font-semibold">Activity</h2>
          <p className="mt-0.5 text-xs text-muted-foreground">
            {trendTotalLabel(metric, total)} across the selected period
          </p>
        </div>
        <div className="flex rounded-lg border border-border-subtle bg-muted/40 p-0.5">
          {(["tokens", "turns", "cost"] as const).map((value) => (
            <button
              key={value}
              type="button"
              data-active={metric === value || undefined}
              className="rounded-md px-2.5 py-1 text-xs font-medium text-muted-foreground transition-colors hover:text-foreground data-[active]:bg-background data-[active]:text-foreground"
              onClick={() => setMetric(value)}
            >
              {value === "cost" ? "Cost" : titleCase(value)}
            </button>
          ))}
        </div>
      </div>
      <div className="px-4 pt-4 pb-3">
        <TrendChart days={days} metric={metric} />
      </div>
    </section>
  );
}

function TrendChart({
  days,
  metric,
}: {
  days: CodeAnalyticsDay[];
  metric: TrendMetric;
}) {
  const values = days.map((day) => trendValue(day, metric));
  const max = Math.max(...values, 1);
  const width = 720;
  const height = 174;
  const top = 10;
  const bottom = 28;
  const plotHeight = height - top - bottom;
  const denominator = Math.max(1, days.length - 1);
  const points = values.map((value, index) => {
    const x = (index / denominator) * width;
    const y = top + plotHeight - (value / max) * plotHeight;
    return { x, y, value, day: days[index] };
  });
  const path = points.map((point) => `${point.x},${point.y}`).join(" ");
  const area = points.length
    ? `0,${top + plotHeight} ${path} ${width},${top + plotHeight}`
    : "";
  const labels = trendLabels(days);
  return (
    <div className="min-w-0" role="img" aria-label={`${metric} trend`}>
      <svg
        viewBox={`0 0 ${width} ${height}`}
        className="h-48 w-full overflow-visible"
        preserveAspectRatio="none"
      >
        {[0, 0.5, 1].map((position) => {
          const y = top + plotHeight * position;
          return (
            <line
              key={position}
              x1="0"
              x2={width}
              y1={y}
              y2={y}
              className="text-border-subtle"
              stroke="currentColor"
              strokeWidth="1"
              vectorEffect="non-scaling-stroke"
            />
          );
        })}
        {area && (
          <polygon points={area} className="text-muted" fill="currentColor" />
        )}
        {path && (
          <polyline
            points={path}
            className="text-foreground"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinejoin="round"
            strokeLinecap="round"
            vectorEffect="non-scaling-stroke"
          />
        )}
        {points.map((point, index) => (
          <circle
            key={`${point.day?.date ?? index}-${metric}`}
            cx={point.x}
            cy={point.y}
            r="2.5"
            className="text-foreground"
            fill="currentColor"
            vectorEffect="non-scaling-stroke"
          >
            <title>{`${point.day?.date ?? ""}: ${formatTrendValue(metric, point.value)}`}</title>
          </circle>
        ))}
        {labels.map((label) => (
          <text
            key={`${label.date}-${label.x}`}
            x={label.x}
            y={height - 5}
            textAnchor={label.anchor}
            className="fill-muted-foreground text-2xs"
          >
            {formatChartDate(label.date)}
          </text>
        ))}
      </svg>
    </div>
  );
}

function QuotaCard({
  quota,
}: {
  quota: ReturnType<typeof useCodeSubscriptionUsage>;
}) {
  const windows = useMemo(() => quotaWindows(quota.report), [quota.report]);
  const gateway = quota.report?.source === "model_gateway";
  return (
    <section className="rounded-xl border border-border bg-background">
      <div className="flex items-start justify-between gap-3 border-b border-border-subtle px-4 py-3.5">
        <div>
          <div className="flex items-center gap-2">
            <h2 className="text-md font-semibold">Subscription limits</h2>
            <span className="rounded-full border border-border-subtle bg-muted/50 px-2 py-0.5 text-2xs font-medium text-muted-foreground">
              {gateway ? "Model Gateway" : "Local"}
            </span>
          </div>
          <p className="mt-0.5 text-xs text-muted-foreground">
            Quotas stay separate from estimated API cost.
          </p>
        </div>
        <button
          type="button"
          className="rounded-md p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
          aria-label="Refresh subscription limits"
          disabled={quota.refreshing}
          onClick={() => void quota.refresh()}
        >
          <RefreshCw
            className={cn("size-3.5", quota.refreshing && "animate-spin")}
          />
        </button>
      </div>
      <div className="p-4">
        {!quota.report && !quota.error ? (
          <div className="flex flex-col gap-3">
            <Skeleton className="h-4 w-28" />
            <Skeleton className="h-2 w-full" />
            <Skeleton className="h-2 w-4/5" />
          </div>
        ) : windows.length > 0 ? (
          <div className="flex flex-col gap-3.5">
            {windows.map((window) => (
              <div key={window.key} className="flex flex-col gap-1.5">
                <div className="flex items-baseline justify-between gap-3 text-xs">
                  <span className="min-w-0 truncate font-medium">
                    {window.provider} · {window.label}
                  </span>
                  <span className="shrink-0 tabular-nums text-muted-foreground">
                    {Math.round(window.used)}% used
                  </span>
                </div>
                <div className="h-1.5 overflow-hidden rounded-full bg-muted">
                  <div
                    className={cn(
                      "h-full rounded-full",
                      window.used >= 100
                        ? "bg-critical"
                        : window.used >= 85
                          ? "bg-warning"
                          : "bg-foreground/55",
                    )}
                    style={{
                      width: `${Math.min(100, Math.max(0, window.used))}%`,
                    }}
                  />
                </div>
              </div>
            ))}
          </div>
        ) : (
          <div className="flex flex-col gap-2">
            <div className="flex items-center gap-2 text-sm font-medium">
              <Gauge className="size-4 text-muted-foreground" />
              Local analytics are ready
            </div>
            <p className="text-xs leading-5 text-muted-foreground">
              When Model Gateway exposes subscription windows, they appear in
              this card. Sessions, tokens, and estimates do not depend on it.
            </p>
            {quota.error && (
              <p className="text-xs text-critical">{quota.error}</p>
            )}
          </div>
        )}
      </div>
    </section>
  );
}

function RepositoryCard({ report }: { report: CodeAnalyticsSnapshot }) {
  return (
    <section className="min-w-0 rounded-xl border border-border bg-background">
      <div className="border-b border-border-subtle px-4 py-3.5">
        <h2 className="text-md font-semibold">Repositories</h2>
        <p className="mt-0.5 text-xs text-muted-foreground">
          Local sessions and attributed pull requests
        </p>
      </div>
      <div className="overflow-x-auto">
        <table className="w-full min-w-[660px] text-left text-xs">
          <thead className="text-muted-foreground">
            <tr className="border-b border-border-subtle">
              <th className="px-4 py-2.5 font-medium">Repository</th>
              <th className="px-3 py-2.5 text-right font-medium">Sessions</th>
              <th className="px-3 py-2.5 text-right font-medium">Turns</th>
              <th className="px-3 py-2.5 text-right font-medium">Tokens</th>
              <th className="px-3 py-2.5 text-right font-medium">PRs</th>
              <th className="px-4 py-2.5 text-right font-medium">Estimate</th>
            </tr>
          </thead>
          <tbody>
            {report.repositories.map((repo) => (
              <tr
                key={repo.repo_id}
                className="border-b border-border-subtle last:border-0"
              >
                <td className="max-w-64 truncate px-4 py-3 font-medium">
                  {repo.name}
                </td>
                <td className="px-3 py-3 text-right tabular-nums text-muted-foreground">
                  {formatNumber(repo.sessions)}
                </td>
                <td className="px-3 py-3 text-right tabular-nums text-muted-foreground">
                  {formatNumber(repo.turns)}
                </td>
                <td className="px-3 py-3 text-right tabular-nums">
                  {formatCompact(repo.total_tokens)}
                </td>
                <td className="px-3 py-3 text-right tabular-nums text-muted-foreground">
                  {repo.pull_requests_opened}
                  <span className="ml-1 text-2xs">
                    · {repo.pull_requests_merged} merged
                  </span>
                </td>
                <td className="px-4 py-3 text-right font-mono tabular-nums">
                  {formatUsdMicros(repo.estimated_cost_microusd)}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}

function ModelCard({ report }: { report: CodeAnalyticsSnapshot }) {
  const maxTokens = Math.max(
    ...report.models.map((model) => model.total_tokens),
    1,
  );
  return (
    <section className="min-w-0 rounded-xl border border-border bg-background">
      <div className="border-b border-border-subtle px-4 py-3.5">
        <h2 className="text-md font-semibold">Models</h2>
        <p className="mt-0.5 text-xs text-muted-foreground">
          Canonical model IDs keep Gateway and local turns together
        </p>
      </div>
      <div className="flex max-h-[390px] flex-col overflow-y-auto p-2">
        {report.models.map((model, index) => (
          <div
            key={`${model.model_id ?? "unknown"}-${model.harness_kind}-${model.fast_mode}-${index}`}
            className="rounded-lg px-2.5 py-2.5 hover:bg-muted/50"
          >
            <div className="flex min-w-0 items-start justify-between gap-3">
              <div className="min-w-0">
                <div className="flex min-w-0 items-center gap-1.5">
                  <span className="truncate font-mono text-xs font-medium">
                    {model.model_id ?? "Unspecified model"}
                  </span>
                  {model.fast_mode && <MiniTag>Fast</MiniTag>}
                  {!model.priced && <MiniTag>Unpriced</MiniTag>}
                </div>
                <p className="mt-0.5 text-2xs text-muted-foreground">
                  {harnessLabel(model.harness_kind)} · {model.turns} turns
                </p>
              </div>
              <div className="shrink-0 text-right">
                <p className="text-xs font-medium tabular-nums">
                  {formatCompact(model.total_tokens)}
                </p>
                <p className="mt-0.5 font-mono text-2xs text-muted-foreground tabular-nums">
                  {model.priced
                    ? formatUsdMicros(model.estimated_cost_microusd)
                    : "—"}
                </p>
              </div>
            </div>
            <div className="mt-2 h-1 overflow-hidden rounded-full bg-muted">
              <div
                className="h-full rounded-full bg-foreground/55"
                style={{ width: `${(model.total_tokens / maxTokens) * 100}%` }}
              />
            </div>
          </div>
        ))}
      </div>
      <div className="border-t border-border-subtle px-4 py-3 text-xs text-muted-foreground">
        Prices dated {report.pricing.prices_as_of}. Third-party routes and fast
        tiers stay unpriced.
      </div>
    </section>
  );
}

function TokenMixCard({ report }: { report: CodeAnalyticsSnapshot }) {
  const totals = report.totals;
  const parts = [
    { label: "Fresh input", value: totals.input_tokens },
    { label: "Cache read", value: totals.cache_read_tokens },
    { label: "Cache write", value: totals.cache_write_tokens },
    { label: "Output", value: totals.output_tokens },
  ];
  return (
    <section className="rounded-xl border border-border bg-background p-4">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-md font-semibold">Token mix</h2>
          <p className="mt-0.5 text-xs text-muted-foreground">
            Spend tokens summed across every model call
          </p>
        </div>
        <span className="font-mono text-xs tabular-nums text-muted-foreground">
          {formatCompact(totals.total_tokens)} total
        </span>
      </div>
      <div className="mt-4 flex h-2 overflow-hidden rounded-full bg-muted">
        {parts.map((part, index) => (
          <div
            key={part.label}
            className={cn(
              index === 0 && "bg-foreground",
              index === 1 && "bg-foreground/65",
              index === 2 && "bg-foreground/40",
              index === 3 && "bg-foreground/20",
            )}
            style={{
              width: `${totals.total_tokens === 0 ? 0 : (part.value / totals.total_tokens) * 100}%`,
            }}
          />
        ))}
      </div>
      <div className="mt-4 grid grid-cols-2 gap-x-5 gap-y-3 sm:grid-cols-4">
        {parts.map((part) => (
          <div key={part.label}>
            <p className="text-2xs font-medium uppercase tracking-wide text-muted-foreground">
              {part.label}
            </p>
            <p className="mt-1 text-sm font-medium tabular-nums">
              {formatCompact(part.value)}
            </p>
          </div>
        ))}
      </div>
    </section>
  );
}

function OutcomeCard({ report }: { report: CodeAnalyticsSnapshot }) {
  const totals = report.totals;
  const terminal =
    totals.completed_turns + totals.failed_turns + totals.interrupted_turns;
  const outcomes = [
    {
      label: "Completed",
      value: totals.completed_turns,
      className: "bg-success",
    },
    { label: "Failed", value: totals.failed_turns, className: "bg-critical" },
    {
      label: "Interrupted",
      value: totals.interrupted_turns,
      className: "bg-warning",
    },
  ];
  return (
    <section className="rounded-xl border border-border bg-background p-4">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-md font-semibold">Turn outcomes</h2>
          <p className="mt-0.5 text-xs text-muted-foreground">
            Finished turns in this period
          </p>
        </div>
        {totals.running_turns > 0 && (
          <span className="rounded-full border border-live-border bg-live-background px-2 py-0.5 text-2xs font-medium text-live-foreground">
            {totals.running_turns} running
          </span>
        )}
      </div>
      <div className="mt-4 flex h-2 overflow-hidden rounded-full bg-muted">
        {outcomes.map((outcome) => (
          <div
            key={outcome.label}
            className={outcome.className}
            style={{
              width: `${terminal === 0 ? 0 : (outcome.value / terminal) * 100}%`,
            }}
          />
        ))}
      </div>
      <div className="mt-4 grid grid-cols-3 gap-3">
        {outcomes.map((outcome) => (
          <div key={outcome.label}>
            <div className="flex items-center gap-1.5">
              <span
                className={cn("size-1.5 rounded-full", outcome.className)}
              />
              <span className="text-2xs font-medium uppercase tracking-wide text-muted-foreground">
                {outcome.label}
              </span>
            </div>
            <p className="mt-1 text-sm font-medium tabular-nums">
              {formatNumber(outcome.value)}
            </p>
          </div>
        ))}
      </div>
    </section>
  );
}

function RangePicker({
  value,
  onChange,
}: {
  value: CodeAnalyticsRange;
  onChange: (value: CodeAnalyticsRange) => void;
}) {
  return (
    <div
      className="flex rounded-lg border border-border-subtle bg-muted/40 p-0.5"
      aria-label="Analytics period"
    >
      {RANGE_OPTIONS.map((option) => (
        <button
          key={option.value}
          type="button"
          data-active={option.value === value || undefined}
          className="rounded-md px-2 py-1 text-xs font-medium text-muted-foreground transition-colors hover:text-foreground data-[active]:bg-background data-[active]:text-foreground"
          onClick={() => onChange(option.value)}
        >
          {option.value === "all" ? "All" : option.value}
        </button>
      ))}
    </div>
  );
}

function AnalyticsSkeleton() {
  return (
    <div
      className="mx-auto flex w-full max-w-[1500px] flex-col gap-4"
      role="status"
    >
      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-6">
        {Array.from({ length: 6 }, (_, index) => (
          <Skeleton key={index} className="h-28 rounded-xl" />
        ))}
      </div>
      <div className="grid gap-4 xl:grid-cols-[minmax(0,1.65fr)_minmax(280px,0.75fr)]">
        <Skeleton className="h-72 rounded-xl" />
        <Skeleton className="h-72 rounded-xl" />
      </div>
      <div className="grid gap-4 xl:grid-cols-[minmax(0,1.2fr)_minmax(0,0.8fr)]">
        <Skeleton className="h-80 rounded-xl" />
        <Skeleton className="h-80 rounded-xl" />
      </div>
    </div>
  );
}

function AnalyticsError({
  message,
  onRetry,
}: {
  message: string;
  onRetry: () => void;
}) {
  return (
    <Empty className="mx-auto h-full max-w-lg">
      <EmptyHeader>
        <EmptyMedia variant="icon">
          <CircleAlert />
        </EmptyMedia>
        <EmptyTitle>Analytics could not load</EmptyTitle>
        <EmptyDescription>{message}</EmptyDescription>
      </EmptyHeader>
      <Button type="button" variant="outline" onClick={onRetry}>
        <RefreshCw />
        Try again
      </Button>
    </Empty>
  );
}

function AnalyticsRefreshError({
  message,
  onRetry,
}: {
  message: string;
  onRetry: () => void;
}) {
  return (
    <div
      className="mx-auto mb-4 flex w-full max-w-[1500px] items-center gap-2 rounded-lg border border-critical-border bg-critical-background px-3 py-2 text-xs text-critical-foreground"
      role="alert"
    >
      <CircleAlert className="size-3.5 shrink-0" />
      <span className="min-w-0 flex-1">{message}</span>
      <button
        type="button"
        className="shrink-0 rounded-md px-2 py-1 font-medium hover:bg-background/60"
        onClick={onRetry}
      >
        Try again
      </button>
    </div>
  );
}

function AnalyticsEmpty({ quota }: { quota: CodeSubscriptionUsage | null }) {
  return (
    <Empty className="mx-auto h-full max-w-xl">
      <EmptyHeader>
        <EmptyMedia variant="icon">
          <ChartNoAxesCombined />
        </EmptyMedia>
        <EmptyTitle>No code activity in this period</EmptyTitle>
        <EmptyDescription>
          Start a code session or choose a longer period. Tidebreak records
          tokens and turn outcomes locally as each turn finishes.
        </EmptyDescription>
      </EmptyHeader>
      <p className="max-w-md text-center text-xs text-muted-foreground">
        {quota?.source === "model_gateway"
          ? "Model Gateway is connected, so subscription limits appear once activity starts."
          : "Model Gateway is optional. Local analytics work without it."}
      </p>
    </Empty>
  );
}

function MiniTag({ children }: { children: string }) {
  return (
    <span className="shrink-0 rounded-full border border-border-subtle bg-muted/50 px-1.5 py-0.5 text-2xs font-medium text-muted-foreground">
      {children}
    </span>
  );
}

function quotaWindows(report: CodeSubscriptionUsage | null) {
  if (!report) return [];
  return report.providers
    .flatMap((provider) => {
      const own = provider.accounts.filter((account) => account.is_own);
      const accounts = own.length > 0 ? own : provider.accounts.slice(0, 1);
      return accounts.flatMap((account) =>
        account.windows.map((window) => ({
          key: `${provider.id}-${account.id}-${window.key}`,
          provider: provider.label,
          label: window.label,
          used: window.used_percent,
        })),
      );
    })
    .sort((left, right) => right.used - left.used)
    .slice(0, 5);
}

function pricingCoverage(report: CodeAnalyticsSnapshot): number | null {
  const total = report.pricing.priced_tokens + report.pricing.unpriced_tokens;
  return total === 0 ? null : report.pricing.priced_tokens / total;
}

function trendValue(day: CodeAnalyticsDay, metric: TrendMetric): number {
  if (metric === "tokens") return day.total_tokens;
  if (metric === "turns") return day.turns;
  return day.estimated_cost_microusd;
}

function trendTotalLabel(metric: TrendMetric, value: number): string {
  if (metric === "tokens") return formatCompact(value);
  if (metric === "turns") return `${formatNumber(value)} turns`;
  return formatUsdMicros(value);
}

function formatTrendValue(metric: TrendMetric, value: number): string {
  return metric === "cost" ? formatUsdMicros(value) : formatCompact(value);
}

function trendLabels(days: CodeAnalyticsDay[]): {
  date: string;
  x: number;
  anchor: "start" | "middle" | "end";
}[] {
  if (days.length === 0) return [];
  const last = days.length - 1;
  const middle = Math.floor(last / 2);
  return [
    { date: days[0]?.date ?? "", x: 0, anchor: "start" },
    {
      date: days[middle]?.date ?? "",
      x: 360,
      anchor: "middle",
    },
    { date: days[last]?.date ?? "", x: 720, anchor: "end" },
  ];
}

function formatChartDate(value: string): string {
  const date = new Date(`${value}T00:00:00Z`);
  return date.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

function formatNumber(value: number): string {
  return new Intl.NumberFormat().format(value);
}

function formatCompact(value: number): string {
  return new Intl.NumberFormat(undefined, {
    notation: "compact",
    maximumFractionDigits: 1,
  }).format(value);
}

function formatUsdMicros(value: number): string {
  const dollars = value / 1_000_000;
  return new Intl.NumberFormat(undefined, {
    style: "currency",
    currency: "USD",
    minimumFractionDigits: dollars < 0.01 ? 3 : 2,
    maximumFractionDigits: dollars < 1 ? 3 : 2,
  }).format(dollars);
}

function formatPercent(value: number | null): string {
  if (value === null) return "—";
  return new Intl.NumberFormat(undefined, {
    style: "percent",
    maximumFractionDigits: 0,
  }).format(value);
}

function rangeLabel(range: CodeAnalyticsRange): string {
  return RANGE_OPTIONS.find((option) => option.value === range)?.label ?? range;
}

function harnessLabel(kind: HarnessKind): string {
  switch (kind) {
    case "claude_code":
      return "Claude Code";
    case "codex":
      return "Codex";
    case "opencode":
      return "opencode";
    case "grok":
      return "Grok";
  }
}

function titleCase(value: string): string {
  return value.charAt(0).toUpperCase() + value.slice(1);
}
