import type { CodeUsage, Diffstat } from "../api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";

/**
 * Turn-boundary card in the code transcript: diffstat, duration, and a
 * link that opens the Diff panel anchored to this turn.
 *
 * The async LLM narrative is deferred; the slot is left empty on purpose.
 */
export function TurnReviewCard({
  status,
  durationMs,
  usage,
  error,
  diffstat,
  onOpenDiff,
}: {
  status: "completed" | "failed" | "interrupted";
  durationMs: number | null;
  usage: CodeUsage | null;
  error: string | null;
  diffstat: Diffstat | null;
  onOpenDiff?: () => void;
}) {
  const label =
    status === "completed"
      ? "Turn completed"
      : status === "failed"
        ? "Turn failed"
        : "Turn interrupted";
  return (
    <section
      className="border-border bg-card max-w-prose rounded-lg border px-3 py-2"
      aria-label="Turn review"
    >
      <div className="flex flex-wrap items-center gap-2 text-xs">
        <span className="font-medium">{label}</span>
        {durationMs !== null && (
          <span className="text-muted-foreground">{formatDuration(durationMs)}</span>
        )}
        {usage && (
          <span className="text-muted-foreground">
            {usage.input_tokens + usage.output_tokens} tokens
          </span>
        )}
        {diffstat && <DiffstatBadge stat={diffstat} />}
        {onOpenDiff && (
          <Button type="button" variant="ghost" size="2xs" onClick={onOpenDiff}>
            Review diff
          </Button>
        )}
      </div>
      {error && (
        <p className="text-critical-foreground mt-1 text-xs">{error}</p>
      )}
    </section>
  );
}

export function DiffstatBadge({ stat }: { stat: Diffstat }) {
  return (
    <Badge variant="outline" size="sm">
      {stat.files} file{stat.files === 1 ? "" : "s"} +{stat.insertions} −
      {stat.deletions}
      {stat.truncated ? " · truncated" : ""}
    </Badge>
  );
}

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}
