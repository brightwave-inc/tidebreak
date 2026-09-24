/**
 * The scripts the debug server plays instead of a model and a coding engine.
 *
 * `TIDEBREAK_SCRIPTED_PROVIDER` replaces model routing for every chat turn:
 * each completion takes the next step, in order, across the whole machine
 * (`crates/tidebreak-server/src/scripted_provider.rs`).
 * `TIDEBREAK_SCRIPTED_HARNESS` stands in for Claude Code in code mode and
 * plays the same events on every turn, writing its files into the worktree
 * first (`crates/tidebreak-server/src/scripted_harness.rs`). Both exist only
 * in debug builds.
 */

/** One model completion: call a tool and wait for its result, or answer and end the turn. */
export type ProviderStep =
  | { tool: string; input: Record<string, unknown> }
  | { text: string };

export type HarnessScript = {
  events: Record<string, unknown>[];
  writes?: { path: string; contents: string }[];
  /** How long the engine holds each turn before it plays anything. */
  turn_delay_ms?: number;
};

/**
 * A code-mode turn: the engine starts, optionally writes files, and replies.
 *
 * `holdMs` keeps every turn running that long before the engine does
 * anything, so a flow can see the page while a turn is still in flight.
 */
export function codeTurn({
  reply,
  writes = [],
  holdMs,
}: {
  reply: string;
  writes?: { path: string; contents: string }[];
  holdMs?: number;
}): HarnessScript {
  return {
    ...(holdMs === undefined ? {} : { turn_delay_ms: holdMs }),
    events: [
      {
        type: "session_started",
        harness_kind: "claude_code",
        harness_version: "scripted",
        resume_ref: "scripted-session",
      },
      { type: "turn_started" },
      { type: "assistant_delta", text: reply },
      {
        type: "turn_completed",
        usage: {
          input_tokens: 4,
          output_tokens: 6,
          cache_read_input_tokens: 0,
          cache_creation_input_tokens: 0,
          context_tokens: 4,
          first_call_context_tokens: 4,
        },
      },
    ],
    writes,
  };
}

/** A chat step that asks to write a file into the conversation's workspace. */
export function writeFile(path: string, content: string): ProviderStep {
  return { tool: "write_file", input: { path, content } };
}
