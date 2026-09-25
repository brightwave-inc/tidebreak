import type { CodeTranscriptItem } from "../code/CodeSessionReducer";
import type { MessageSearchHit, ToolDetail } from "../generated/wire";
import type { ChatMessage } from "../MessageList";
import { termSpans } from "./highlightTerms";
import { foldText } from "./messageSearch";

/**
 * Finding in what a transcript has loaded, for a conversation the message
 * index does not search for you: a chat with memory incognito on, which the
 * index leaves out, and a code session someone else owns, since a search
 * reads only your own conversations. The find bar searches the rows the
 * transcript holds instead, and says why that is all it searches.
 */

/** Why the find bar searches only what an incognito chat has loaded. */
export const INCOGNITO_FIND_NOTE =
  "Incognito conversations are kept out of search, so this finds only in the messages loaded here.";

/** Why the find bar searches only what a session shared with you has loaded. */
export const SHARED_FIND_NOTE =
  "Search covers only your own conversations, so this finds only in what is loaded here.";

/**
 * Whether `text` holds every one of `terms`, each at the start of a word or
 * a camelCase part of one: the matches the transcript marks.
 */
export function holdsEveryTerm(
  text: string,
  terms: readonly string[],
): boolean {
  if (terms.length === 0) return false;
  const folded = foldText(text);
  return terms.every(
    (term) => folded.includes(term) && termSpans(text, [term]).length > 0,
  );
}

function hit(
  kind: MessageSearchHit["kind"],
  sessionId: string,
  fields: Pick<
    MessageSearchHit,
    "source" | "message_id" | "turn_id" | "event_seq"
  >,
  createdAt: string | undefined,
): MessageSearchHit {
  return {
    kind,
    session_id: sessionId,
    ...fields,
    snippet: "",
    ranges: [],
    created_at: createdAt ?? "",
    archived: false,
  };
}

/** The loaded messages of a chat that hold every term, newest first. */
export function loadedChatMatches(
  sessionId: string,
  messages: readonly ChatMessage[],
  terms: readonly string[],
): MessageSearchHit[] {
  const matches: MessageSearchHit[] = [];
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (message?.role !== "user" && message?.role !== "assistant") continue;
    if (!holdsEveryTerm(message.text, terms)) continue;
    matches.push(
      hit(
        "chat",
        sessionId,
        {
          source: message.role,
          message_id: message.id,
          turn_id: message.turnId,
        },
        message.createdAt,
      ),
    );
  }
  return matches;
}

/** What a tool call acted on, the way the index reads it. */
function toolSubject(detail: ToolDetail): string {
  switch (detail.kind) {
    case "command":
      return detail.cmd;
    case "file_edit":
    case "file_read":
      return detail.path;
    case "search":
      return detail.query;
    case "other":
      return detail.summary;
  }
}

/**
 * The rows a code session's transcript shows that hold every term, newest
 * first: prompts, messages, steers, and what each tool call acted on. A row
 * that no journal event names cannot be opened, so it is left out.
 */
export function loadedCodeMatches(
  sessionId: string,
  rows: readonly CodeTranscriptItem[],
  terms: readonly string[],
): MessageSearchHit[] {
  const matches: MessageSearchHit[] = [];
  for (let index = rows.length - 1; index >= 0; index -= 1) {
    const item = rows[index];
    if (!item) continue;
    if (item.kind === "user") {
      if (holdsEveryTerm(item.text, terms)) {
        matches.push(
          hit(
            "code",
            sessionId,
            { source: "user", turn_id: item.turnId },
            item.createdAt,
          ),
        );
      }
      continue;
    }
    if (
      item.kind !== "assistant" &&
      item.kind !== "steer" &&
      item.kind !== "tool"
    ) {
      continue;
    }
    const seq = item.seqs?.[0];
    if (seq === undefined) continue;
    const text =
      item.kind === "tool"
        ? `${item.name} ${toolSubject(item.detail)}`
        : item.kind === "assistant" && item.rewrite
          ? `${item.text} ${item.rewrite}`
          : item.text;
    if (!holdsEveryTerm(text, terms)) continue;
    matches.push(
      hit(
        "code",
        sessionId,
        {
          source:
            item.kind === "tool"
              ? "tool"
              : item.kind === "steer"
                ? "user"
                : "assistant",
          event_seq: seq,
          turn_id: item.turnId ?? undefined,
        },
        undefined,
      ),
    );
  }
  return matches;
}
