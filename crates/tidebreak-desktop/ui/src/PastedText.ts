export const LONG_PASTE_MIN_CHARACTERS = 1_000;

/** One long clipboard paste held outside the editable composer text. */
export type PastedTextAttachment = {
  id: string;
  text: string;
};

/** Short clipboard text stays editable. Long text becomes message context. */
export function shouldAttachPastedText(text: string): boolean {
  return [...text.trim()].length >= LONG_PASTE_MIN_CHARACTERS;
}

/** Put held paste context back into the plain-text message sent to the model. */
export function messageWithPastedText(
  draft: string,
  items: readonly PastedTextAttachment[],
): string {
  const parts = items.map(
    (item) => `<pasted_text>\n${item.text}\n</pasted_text>`,
  );
  return [draft.trim(), ...parts].filter(Boolean).join("\n\n");
}

export function pastedTextLineCount(text: string): number {
  return text.split(/\r\n|\r|\n/).length;
}

export function pastedTextPreview(text: string): string {
  return (
    text
      .split(/\r\n|\r|\n/)
      .map((line) => line.trim())
      .find(Boolean) ?? "Pasted text"
  );
}

/** A sent message, with each held paste separated back out of it. */
export type SplitPastedText = {
  /** What the reader typed, with the paste blocks removed. */
  prose: string;
  pasted: string[];
};

const OPEN_TAG = "<pasted_text>";
const CLOSE_TAG = "</pasted_text>";

/**
 * Take the paste blocks `messageWithPastedText` added back out of a message,
 * so a transcript can fold them the way the composer chip did.
 *
 * Blocks nest: a debug report pasted by Uneff me carries the source session's
 * own turns, and those may hold wrappers from earlier pastes. A block ends at
 * the closer that balances its opener, not at the first closer in sight, and
 * an opener that never balances runs to the end of the message rather than
 * spilling the rest of the paste into the prose.
 */
export function splitPastedText(message: string): SplitPastedText {
  const pasted: string[] = [];
  let prose = "";
  let cursor = 0;
  while (cursor < message.length) {
    const open = message.indexOf(OPEN_TAG, cursor);
    if (open === -1) {
      prose += message.slice(cursor);
      break;
    }
    prose += message.slice(cursor, open);
    let depth = 1;
    let scan = open + OPEN_TAG.length;
    let close = -1;
    while (depth > 0) {
      const nextOpen = message.indexOf(OPEN_TAG, scan);
      const nextClose = message.indexOf(CLOSE_TAG, scan);
      if (nextClose === -1) break;
      if (nextOpen !== -1 && nextOpen < nextClose) {
        depth += 1;
        scan = nextOpen + OPEN_TAG.length;
        continue;
      }
      depth -= 1;
      scan = nextClose + CLOSE_TAG.length;
      if (depth === 0) close = nextClose;
    }
    const bodyEnd = close === -1 ? message.length : close;
    pasted.push(
      trimBlockNewlines(message.slice(open + OPEN_TAG.length, bodyEnd)),
    );
    cursor = close === -1 ? message.length : close + CLOSE_TAG.length;
  }
  return { prose: prose.replace(/\n{3,}/g, "\n\n").trim(), pasted };
}

/** The wrapper adds one newline on each side of the body; take only those. */
function trimBlockNewlines(body: string): string {
  return body.replace(/^\n/, "").replace(/\n$/, "");
}
