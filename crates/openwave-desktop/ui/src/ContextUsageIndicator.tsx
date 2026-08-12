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
 * Renders nothing at all when there is nothing honest to say — no turn has
 * finished in this chat, or the selected model's window is unknown. A meter
 * that guesses is worse than no meter.
 *
 * The denominator is the *currently selected* model, not the one that ran the
 * turn. Switching models mid-chat is a question about what will fit next, so
 * the reading moves the moment the selection does.
 *
 * Lives in the chat header beside the activity chip so the reading stays on
 * screen for the whole conversation, not only while the composer is in view.
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
  const percent = contextUsagePercent(usage, contextWindow);
  if (percent === null || contextWindow === undefined) return null;

  const used = contextTokens(usage);
  const level = contextUsageLevel(percent);
  const parts = [
    { label: "In", tokens: usage.input_tokens },
    { label: "Out", tokens: usage.output_tokens },
    { label: "Cache read", tokens: usage.cache_read_input_tokens },
    { label: "Cache write", tokens: usage.cache_creation_input_tokens },
  ].filter((part) => part.tokens > 0);

  return (
    <WithTooltip
      contentClassName="max-w-none px-3.5 py-3"
      label={
        <div className="w-52 space-y-2.5 text-left font-normal">
          {modelName && (
            <div className="truncate text-xs font-medium leading-tight">
              {modelName}
            </div>
          )}
          <div className="flex items-end justify-between gap-3">
            <div className="font-mono text-2xl leading-none font-semibold tabular-nums tracking-tight">
              {percent}
              <span className="ml-0.5 text-sm font-medium opacity-60">%</span>
            </div>
            <div className="pb-0.5 text-right font-mono text-[11px] leading-snug tabular-nums opacity-80">
              <div>
                {formatTokenCount(used)}
                <span className="mx-0.5 opacity-50">/</span>
                {formatTokenCount(contextWindow)}
              </div>
              <div className="text-[10px] font-sans opacity-70">context used</div>
            </div>
          </div>
          <div
            className="h-1 w-full overflow-hidden rounded-full bg-primary-foreground/20"
            aria-hidden="true"
          >
            <div
              className="h-full rounded-full bg-primary-foreground/90"
              style={{ width: `${percent}%` }}
            />
          </div>
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
      <span
        className={cn(
          "flex h-7 shrink-0 items-center gap-1.5 rounded-full border px-2.5 text-xs font-medium tabular-nums",
          level === "critical"
            ? "border-destructive/40 bg-destructive/10 text-destructive"
            : level === "warning"
              ? "border-warning-border bg-warning-background text-warning-foreground"
              : "border-border bg-muted text-foreground",
        )}
        // The visible chip is a magnitude; the accessible name is the sentence
        // a screen reader needs, since "34%" alone says nothing about of what.
        // A graphic with a text alternative rather than a live region: this
        // updates on every turn, and it is reference material, not an
        // announcement worth interrupting for.
        role="img"
        aria-label={`Context: ${percent}% of ${formatTokenCount(contextWindow)} tokens used`}
      >
        <ContextUsageRing percent={percent} />
        <span>{percent}%</span>
        <span className="font-normal opacity-70">{formatTokenCount(used)}</span>
      </span>
    </WithTooltip>
  );
}

/**
 * A filled ring, drawn as a conic sweep over a muted track.
 *
 * Takes its colour from the chip via `currentColor`, so the threshold styling
 * lives in exactly one place.
 */
function ContextUsageRing({ percent }: { percent: number }) {
  const filled = `${percent * 3.6}deg`;
  return (
    <span
      aria-hidden="true"
      className="size-3.5 shrink-0 rounded-full"
      style={{
        background: `conic-gradient(currentColor 0deg ${filled}, color-mix(in oklch, currentColor 22%, transparent) ${filled} 360deg)`,
      }}
    />
  );
}
