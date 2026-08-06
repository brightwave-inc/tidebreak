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
  const cached =
    usage.cache_read_input_tokens + usage.cache_creation_input_tokens > 0;

  return (
    <WithTooltip
      label={
        <div className="space-y-0.5 text-xs">
          {modelName && <div className="font-medium">{modelName}</div>}
          <div>
            {used.toLocaleString()} of {contextWindow.toLocaleString()} tokens (
            {percent}%)
          </div>
          <div className="text-muted-foreground">
            {usage.input_tokens.toLocaleString()} in ·{" "}
            {usage.output_tokens.toLocaleString()} out
            {cached && (
              <>
                {" "}
                · {usage.cache_read_input_tokens.toLocaleString()} cache read ·{" "}
                {usage.cache_creation_input_tokens.toLocaleString()} cache write
              </>
            )}
          </div>
        </div>
      }
    >
      <span
        className={cn(
          "flex h-8 shrink-0 items-center gap-1.5 rounded-md px-2 text-xs tabular-nums",
          level === "critical"
            ? "text-destructive"
            : level === "warning"
              ? "text-warning-foreground"
              : "text-muted-foreground",
        )}
        // The visible pill is a magnitude; the accessible name is the sentence
        // a screen reader needs, since "34%" alone says nothing about of what.
        // A graphic with a text alternative rather than a live region: this
        // updates on every turn, and it is reference material, not an
        // announcement worth interrupting for.
        role="img"
        aria-label={`Context: ${percent}% of ${formatTokenCount(contextWindow)} tokens used`}
      >
        <ContextUsageRing percent={percent} />
        {percent}%
      </span>
    </WithTooltip>
  );
}

/**
 * A small filled ring, drawn as a conic sweep over a muted track.
 *
 * Takes its colour from the pill via `currentColor`, so the threshold styling
 * lives in exactly one place.
 */
function ContextUsageRing({ percent }: { percent: number }) {
  const filled = `${percent * 3.6}deg`;
  return (
    <span
      aria-hidden="true"
      className="size-3 shrink-0 rounded-full"
      style={{
        background: `conic-gradient(currentColor 0deg ${filled}, color-mix(in oklch, currentColor 20%, transparent) ${filled} 360deg)`,
      }}
    />
  );
}
