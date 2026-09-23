import type { ChatTranscript } from "../api";
import type { ChatMessage } from "../MessageList";

/**
 * Synthetic transcripts for the chat performance benchmark.
 *
 * Generated rather than committed: the shapes are what matter, and a long
 * conversation is thousands of rows nobody should have to review.
 */

const PARAGRAPH =
  "The resolver keeps a map from package name to node, and the cache stores that map keyed by the lockfile hash, so a warm start skips the walk entirely. ";

const CODE_LINE =
  "export function resolve(graph: Graph, name: string): Node | undefined {\n  return graph.nodes.get(name) ?? graph.aliases.get(name);\n}\n";

/** A Markdown answer of at least `bytes` characters, shaped like real output. */
export function markdownAnswer(turn: number, bytes: number): string {
  let text =
    `## Step ${turn}: what changed\n\n` +
    `${PARAGRAPH.repeat(2)}\n\n` +
    "- Reads the lockfile once per run\n- Skips packages whose hash did not move\n- Writes the cache atomically\n\n" +
    "```ts\n" +
    CODE_LINE.repeat(3) +
    "```\n\n" +
    "| Package | Before | After |\n| --- | --- | --- |\n| core | 4.2 s | 1.1 s |\n| cli | 2.0 s | 0.4 s |\n\n";
  let paragraph = 0;
  while (text.length < bytes) {
    paragraph += 1;
    text += `Paragraph ${paragraph}: ${PARAGRAPH}\n\n`;
  }
  return text.trimEnd();
}

/**
 * A conversation of `turns` exchanges: the question, `toolsPerTurn` settled
 * tool calls, and a Markdown answer of about `answerBytes` characters.
 */
export function syntheticTranscript({
  turns,
  toolsPerTurn,
  answerBytes,
}: {
  turns: number;
  toolsPerTurn: number;
  answerBytes: number;
}): ChatMessage[] {
  const tools = ["search", "read_document", "web_search", "read_file"];
  const messages: ChatMessage[] = [];
  for (let turn = 0; turn < turns; turn += 1) {
    const at = new Date(Date.UTC(2026, 8, 1, 0, turn)).toISOString();
    messages.push({
      id: `user-${turn}`,
      role: "user",
      text: `Question ${turn}: how does the resolver cache the dependency graph between runs?`,
      createdAt: at,
    });
    for (let call = 0; call < toolsPerTurn; call += 1) {
      messages.push({
        id: `tool-${turn}-${call}`,
        role: "tool",
        callId: `call-${turn}-${call}`,
        name: tools[call % tools.length]!,
        status: "completed",
      });
    }
    messages.push({
      id: `assistant-${turn}`,
      role: "assistant",
      text: markdownAnswer(turn, answerBytes),
      sources: [],
      createdAt: at,
    });
  }
  return messages;
}

/** Prose followed by a code fence of `lines` lines that has not closed yet. */
export function streamingFence(lines: number): string {
  let body = "";
  for (let line = 0; line < lines; line += 1) {
    body += `  const value${line} = resolve(graph, "package-${line}") ?? fallback(${line});\n`;
  }
  return `${markdownAnswer(0, 1_500)}\n\nHere is the whole module:\n\n\`\`\`ts\n${body}`;
}

const NO_USAGE = {
  input_tokens: 0,
  output_tokens: 0,
  cache_read_input_tokens: 0,
  cache_creation_input_tokens: 0,
};

/**
 * The newest `pageTurns` turns of a `turns`-turn conversation, shaped as the
 * messages route sends them: durable messages, finished tool calls, and a
 * finished-turn record per turn.
 */
export function syntheticWireTranscript({
  turns,
  toolsPerTurn,
  answerBytes,
  pageTurns,
}: {
  turns: number;
  toolsPerTurn: number;
  answerBytes: number;
  pageTurns: number;
}): ChatTranscript {
  const tools = ["search", "read_document", "web_search", "read_file"] as const;
  const first = Math.max(0, turns - pageTurns);
  const at = (turn: number, second: number) =>
    new Date(Date.UTC(2026, 8, 1, 0, turn, second)).toISOString();
  const transcript: ChatTranscript = {
    messages: [],
    tool_activity: [],
    terminal_turns: [],
    last_event_seq: turns * 10,
    has_more: first > 0,
    earlier_cursor: first > 0 ? first * 2 + 1 : null,
    answer_versions: [],
  };
  for (let turn = first; turn < turns; turn += 1) {
    transcript.messages.push({
      id: `user-${turn}`,
      turn_id: "turn-fixture",
      role: "user",
      content: `Question ${turn}: how does the resolver cache the dependency graph between runs?`,
      created_at: at(turn, 0),
      citations: [],
    });
    for (let call = 0; call < toolsPerTurn; call += 1) {
      transcript.tool_activity.push({
        call_id: `call-${turn}-${call}`,
        turn_id: "turn-fixture",
        tool: tools[call % tools.length]!,
        result_unreadable: false,
        status: "completed",
        started_at: at(turn, 1 + call),
        finished_at: at(turn, 2 + call),
      });
    }
    transcript.messages.push({
      id: `assistant-${turn}`,
      turn_id: "turn-fixture",
      role: "assistant",
      content: markdownAnswer(turn, answerBytes),
      created_at: at(turn, 30),
      citations: [],
    });
    transcript.terminal_turns.push({
      turn_id: `turn-${turn}`,
      message_id: `assistant-${turn}`,
      status: "completed",
      partial_content: "",
      file_changes: [],
      memory_proposals: [],
      usage: NO_USAGE,
      voice_input_used: false,
      finished_at: at(turn, 31),
    });
  }
  return transcript;
}
