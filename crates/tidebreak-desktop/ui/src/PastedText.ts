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
