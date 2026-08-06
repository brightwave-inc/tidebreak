import type { RendererTurnUsage } from "./generated/wire";

/**
 * How much of a model's context window the last turn accounted for.
 *
 * The four counts on [`RendererTurnUsage`] are disjoint — `input_tokens` is the
 * fresh prompt only, and the two cache figures are the portions read from and
 * written to the provider's prompt cache — so the tokens that occupied the
 * window are their plain sum. See the generated type's own doc comment for why
 * that holds on every provider, and for the caveat that these are turn totals
 * summed across model calls rather than a snapshot of the final prompt.
 */
export function contextTokens(usage: RendererTurnUsage): number {
  return (
    usage.input_tokens +
    usage.output_tokens +
    usage.cache_read_input_tokens +
    usage.cache_creation_input_tokens
  );
}

/**
 * Share of the window used, as a whole percent clamped to 0–100.
 *
 * Clamped rather than allowed to run over: a bar reading 130% tells a reader
 * nothing they can act on beyond what "full" already tells them, and the turn
 * totals can legitimately exceed the window on a long multi-step turn.
 *
 * Returns null when the denominator is unusable, which is the signal to render
 * nothing at all — a model whose context window we do not know cannot be
 * honestly metered.
 */
export function contextUsagePercent(
  usage: RendererTurnUsage,
  contextWindow: number | undefined,
): number | null {
  if (!contextWindow || contextWindow <= 0) return null;
  const used = contextTokens(usage);
  return Math.min(100, Math.max(0, Math.round((used / contextWindow) * 100)));
}

/** How loudly the meter should read at a given fill. */
export type ContextUsageLevel = "normal" | "warning" | "critical";

export function contextUsageLevel(percent: number): ContextUsageLevel {
  if (percent >= 90) return "critical";
  if (percent >= 75) return "warning";
  return "normal";
}

/**
 * Token counts at the scale people quote them: "200k", "1.5M", "840".
 *
 * Deliberately lossy. These numbers exist to convey magnitude — the exact
 * figures belong in the tooltip, which prints them in full.
 */
export function formatTokenCount(tokens: number): string {
  if (tokens >= 1_000_000) {
    const millions = tokens / 1_000_000;
    return `${millions >= 10 || Number.isInteger(millions) ? Math.round(millions) : millions.toFixed(1)}M`;
  }
  if (tokens >= 1_000) return `${Math.round(tokens / 1_000)}k`;
  return `${tokens}`;
}

/**
 * The trimming notice, carrying the sizes that make it worth reading.
 *
 * "Earlier conversation was trimmed" on its own leaves the reader unable to
 * tell a trivial trim from one that dropped most of the conversation.
 */
export function contextTruncationNotice(
  originalTokens: number,
  fittedTokens: number,
): string {
  if (originalTokens <= 0 || fittedTokens <= 0 || fittedTokens >= originalTokens) {
    return "Earlier conversation was trimmed to fit the model's context.";
  }
  return `Earlier conversation was trimmed to fit the model's context (~${formatTokenCount(originalTokens)} → ~${formatTokenCount(fittedTokens)} tokens).`;
}
