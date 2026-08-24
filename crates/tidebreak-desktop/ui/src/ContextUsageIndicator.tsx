import {
  contextUsageLevel,
  contextUsagePercent,
  formatTokenCount,
} from "./ContextUsage";
import { WithTooltip } from "./components/ui/tooltip";
import { cn } from "./lib/utils";

/**
 * The two readings a finished turn produces, mapped by whoever has them.
 *
 * Deliberately not a wire type. Code mode reads occupancy from the engine's
 * last model call; chat has only turn totals. Naming both here forces each
 * surface to say which number it is handing over, instead of passing a usage
 * struct whose four counts get quietly reinterpreted as occupancy.
 */
export type ContextUsageReading = {
  /**
   * Prompt tokens resident on the last model call — the ring's numerator.
   *
   * Null when the engine published nothing that answers "how full is the
   * window". The ring then shows no fill rather than a made-up one.
   */
  contextTokens: number | null;
  /** The turn's token spend, summed across every model call it made. */
  spend: {
    input: number;
    output: number;
    cacheRead: number;
    cacheWrite: number;
  };
  contextWindow: number | undefined;
  modelName: string | undefined;
};

/**
 * How much of the active model's context window the last turn left resident.
 *
 * The ring shows context; the hover shows spend. They are different numbers:
 * a turn that ran six model calls spent roughly six prompts' worth of tokens
 * while the window still only held one, so summing the spend to fill the ring
 * reads several times too high and pins a healthy session at "full".
 *
 * An unknown window still shows the resident tokens — a missing denominator is
 * not a missing reading. An engine that publishes no per-call figure gets no
 * fill at all, which is the honest presentation of "not measured".
 *
 * The denominator is the *currently selected* model, not the one that ran the
 * turn. Switching models mid-chat is a question about what will fit next, so
 * the reading moves the moment the selection does.
 *
 * Lives in the composer's send cluster as a ring, not a labeled chip: the
 * magnitude is a glance, and the numbers wait on hover.
 */
export function ContextUsageIndicator({
  contextTokens,
  spend,
  contextWindow,
  modelName,
}: ContextUsageReading) {
  // Zero is not a reading. An engine that publishes nothing usable reports
  // zero, and filling a ring from it would read as an empty window rather
  // than an unmeasured one.
  const resident = contextTokens && contextTokens > 0 ? contextTokens : null;
  const percent =
    resident === null ? null : contextUsagePercent(resident, contextWindow);
  const metered =
    resident !== null && percent !== null && contextWindow !== undefined;
  const level = metered ? contextUsageLevel(percent) : "normal";
  const parts = [
    { label: "In", tokens: spend.input },
    { label: "Out", tokens: spend.output },
    { label: "Cache read", tokens: spend.cacheRead },
    { label: "Cache write", tokens: spend.cacheWrite },
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
            <div className="shrink-0 font-mono text-xs tabular-nums opacity-80">
              {metered ? (
                <>
                  {percent}%<span className="mx-1 opacity-50">·</span>
                  {formatTokenCount(resident)}
                  <span className="mx-0.5 opacity-50">/</span>
                  {formatTokenCount(contextWindow)}
                </>
              ) : resident !== null ? (
                <>{resident.toLocaleString()} tokens</>
              ) : null}
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
            <p className="text-xs leading-snug opacity-70">
              {resident === null
                ? "This engine reports no context reading"
                : "No published context window"}
            </p>
          )}
          <dl className="grid grid-cols-[1fr_auto] gap-x-4 gap-y-0.5 border-t border-primary-foreground/15 pt-2 text-xs leading-relaxed">
            <div className="col-span-2 grid grid-cols-subgrid">
              <dt className="opacity-70">Context</dt>
              <dd className="font-mono tabular-nums">
                {resident === null
                  ? "—"
                  : contextWindow
                    ? `${resident.toLocaleString()} / ${contextWindow.toLocaleString()}`
                    : resident.toLocaleString()}
              </dd>
            </div>
          </dl>
          {parts.length > 0 && (
            <div className="space-y-1 border-t border-primary-foreground/15 pt-2">
              {/* Labeled, because these are not the ring's numbers: they sum
                  every model call the turn made, and on a long turn they run
                  well past what the window ever held. */}
              <p className="text-2xs uppercase tracking-wide opacity-60">
                Turn spend
              </p>
              <dl className="grid grid-cols-[1fr_auto] gap-x-4 gap-y-0.5 text-xs leading-relaxed">
                {parts.map((part) => (
                  <div
                    key={part.label}
                    className="col-span-2 grid grid-cols-subgrid"
                  >
                    <dt className="opacity-70">{part.label}</dt>
                    <dd className="font-mono tabular-nums">
                      {part.tokens.toLocaleString()}
                    </dd>
                  </div>
                ))}
              </dl>
            </div>
          )}
        </div>
      }
    >
      <button
        type="button"
        className={cn(
          "inline-flex size-7 shrink-0 items-center justify-center rounded-full text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring",
          level === "critical" && "text-destructive hover:text-destructive",
          level === "warning" &&
            "text-warning-foreground hover:text-warning-foreground",
        )}
        // A graphic with a text alternative rather than a live region: this
        // updates on every turn, and it is reference material, not an
        // announcement worth interrupting for.
        aria-label={
          metered
            ? `Context: ${percent}% of ${formatTokenCount(contextWindow)} tokens used`
            : resident !== null
              ? `Context: ${resident.toLocaleString()} tokens used`
              : "Context: no reading from this engine"
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
    <svg viewBox="0 0 20 20" className="size-5 -rotate-90" aria-hidden="true">
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
