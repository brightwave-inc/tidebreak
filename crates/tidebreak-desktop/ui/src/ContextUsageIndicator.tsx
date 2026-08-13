import type { RendererTurnUsage } from "./generated/wire";
import {
  contextTokens,
  contextUsageLevel,
  contextUsagePercent,
  formatTokenCount,
} from "./ContextUsage";
import { WithTooltip } from "./components/ui/tooltip";
import { cn } from "./lib/utils";

/**
 * How much of the active model's context window the last turn accounted for.
 *
 * Renders nothing only when no turn has finished. An unknown window still
 * shows the used tokens — a missing denominator is not a missing reading.
 *
 * The denominator is the *currently selected* model, not the one that ran the
 * turn. Switching models mid-chat is a question about what will fit next, so
 * the reading moves the moment the selection does.
 *
 * Lives in the composer's send cluster as a ring, not a labeled chip: the
 * magnitude is a glance, and the numbers wait on hover.
 */
export function ContextUsageIndicator({
  usage,
  contextWindow,
  modelName,
}: {
  usage: RendererTurnUsage | null;
  contextWindow: number | undefined;
  modelName: string | undefined;
}) {
  if (!usage) return null;

  const used = contextTokens(usage);
  const percent = contextUsagePercent(usage, contextWindow);
  const metered = percent !== null && contextWindow !== undefined;
  const level = metered ? contextUsageLevel(percent) : "normal";
  const parts = [
    { label: "In", tokens: usage.input_tokens },
    { label: "Out", tokens: usage.output_tokens },
    { label: "Cache read", tokens: usage.cache_read_input_tokens },
    { label: "Cache write", tokens: usage.cache_creation_input_tokens },
  ].filter((part) => part.tokens > 0);

  return (
    <WithTooltip
      side="top"
      align="end"
      collisionPadding={12}
      contentClassName="max-w-none max-h-[min(20rem,calc(100vh-1.5rem))] overflow-y-auto px-3.5 py-3"
      label={
        <div className="w-56 space-y-2.5 text-left font-normal">
          <div className="flex items-center justify-between gap-3">
            <div className="truncate text-xs font-medium leading-tight">
              {modelName ?? "Context window"}
            </div>
            <div className="shrink-0 font-mono text-[11px] tabular-nums opacity-80">
              {metered ? (
                <>
                  {percent}%
                  <span className="mx-1 opacity-50">·</span>
                  {formatTokenCount(used)}
                  <span className="mx-0.5 opacity-50">/</span>
                  {formatTokenCount(contextWindow)}
                </>
              ) : (
                <>{used.toLocaleString()} tokens</>
              )}
            </div>
          </div>
          {metered ? (
            <div
              className="h-1.5 w-full overflow-hidden rounded-full bg-primary-foreground/20"
              aria-hidden="true"
            >
              <div
                className={cn(
                  "h-full rounded-full",
                  level === "critical"
                    ? "bg-destructive"
                    : "bg-primary-foreground/90",
                )}
                style={{ width: `${percent}%` }}
              />
            </div>
          ) : (
            <p className="text-[11px] leading-snug opacity-70">
              No published context window
            </p>
          )}
          {parts.length > 0 && (
            <dl className="grid grid-cols-[1fr_auto] gap-x-4 gap-y-0.5 border-t border-primary-foreground/15 pt-2 text-[11px] leading-relaxed">
              {parts.map((part) => (
                <div key={part.label} className="col-span-2 grid grid-cols-subgrid">
                  <dt className="opacity-70">{part.label}</dt>
                  <dd className="font-mono tabular-nums">
                    {part.tokens.toLocaleString()}
                  </dd>
                </div>
              ))}
            </dl>
          )}
        </div>
      }
    >
      <button
        type="button"
        className={cn(
          "inline-flex size-7 shrink-0 items-center justify-center rounded-full text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring",
          level === "critical" && "text-destructive hover:text-destructive",
          level === "warning" && "text-warning-foreground hover:text-warning-foreground",
        )}
        // A graphic with a text alternative rather than a live region: this
        // updates on every turn, and it is reference material, not an
        // announcement worth interrupting for.
        aria-label={
          metered
            ? `Context: ${percent}% of ${formatTokenCount(contextWindow)} tokens used`
            : `Context: ${used.toLocaleString()} tokens used`
        }
      >
        <ContextUsageRing percent={metered ? percent : null} />
      </button>
    </WithTooltip>
  );
}

/**
 * A stroked ring. Colour comes from the button via `currentColor`, so the
 * warning and critical states live on the trigger, not here. No fill when
 * there is no honest percent — the track still marks the slot.
 */
function ContextUsageRing({ percent }: { percent: number | null }) {
  const radius = 7.25;
  const circumference = 2 * Math.PI * radius;
  const offset =
    percent === null
      ? undefined
      : circumference * (1 - Math.min(100, Math.max(0, percent)) / 100);
  return (
    <svg
      viewBox="0 0 20 20"
      className="size-5 -rotate-90"
      aria-hidden="true"
    >
      <circle
        cx="10"
        cy="10"
        r={radius}
        fill="none"
        stroke="currentColor"
        strokeOpacity="0.22"
        strokeWidth="2.5"
      />
      {offset !== undefined && (
        <circle
          cx="10"
          cy="10"
          r={radius}
          fill="none"
          stroke="currentColor"
          strokeWidth="2.5"
          strokeLinecap="round"
          strokeDasharray={circumference}
          strokeDashoffset={offset}
        />
      )}
    </svg>
  );
}
