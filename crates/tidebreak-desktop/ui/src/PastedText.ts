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

const PASTED_TEXT_BLOCK = /<pasted_text>\n?([\s\S]*?)\n?<\/pasted_text>/g;

/**
 * Take the paste blocks `messageWithPastedText` added back out of a message,
 * so a transcript can fold them the way the composer chip did.
 */
export function splitPastedText(message: string): SplitPastedText {
  const pasted: string[] = [];
  const prose = message
    .replace(PASTED_TEXT_BLOCK, (_match, body: string) => {
      pasted.push(body);
      return "";
    })
    .replace(/\n{3,}/g, "\n\n")
    .trim();
  return { prose, pasted };
}
