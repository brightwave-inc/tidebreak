import type { ChatTerminalTurnSnapshot, RendererTurnUsage } from "./generated/wire";

/**
 * Every token this chat has spent, summed over its finished turns.
 *
 * A running total answers a different question from the context meter: the
 * meter reads the last turn because each turn re-sends the conversation, while
 * this is what the chat has cost to run so far. Both are token counts and
 * neither is money — the app deliberately quotes no prices, because the rate a
 * given key is billed at is not something the renderer can know.
 *
 * Cancelled and failed turns count: their tokens were spent whether or not the
 * turn produced an answer.
 */
export function chatUsageTotals(
  turns: readonly ChatTerminalTurnSnapshot[] | undefined,
): RendererTurnUsage & { turns: number } {
  const total = {
    input_tokens: 0,
    output_tokens: 0,
    cache_read_input_tokens: 0,
    cache_creation_input_tokens: 0,
    turns: 0,
  };
  for (const turn of turns ?? []) {
    total.input_tokens += turn.usage.input_tokens;
    total.output_tokens += turn.usage.output_tokens;
    total.cache_read_input_tokens += turn.usage.cache_read_input_tokens;
    total.cache_creation_input_tokens += turn.usage.cache_creation_input_tokens;
    total.turns += 1;
  }
  return total;
}
