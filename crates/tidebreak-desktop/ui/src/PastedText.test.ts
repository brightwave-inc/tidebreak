import { describe, expect, it } from "vitest";

import {
  LONG_PASTE_MIN_CHARACTERS,
  messageWithPastedText,
  pastedTextLineCount,
  pastedTextPreview,
  shouldAttachPastedText,
} from "./PastedText";

describe("PastedText", () => {
  it("keeps short paste editable and attaches long paste", () => {
    expect(shouldAttachPastedText("short note")).toBe(false);
    expect(shouldAttachPastedText("x".repeat(LONG_PASTE_MIN_CHARACTERS))).toBe(
      true,
    );
  });

  it("adds held paste context after the typed instruction", () => {
    expect(
      messageWithPastedText("Summarize this", [
        { id: "paste-1", text: "First line\nSecond line" },
      ]),
    ).toBe(
      "Summarize this\n\n<pasted_text>\nFirst line\nSecond line\n</pasted_text>",
    );
  });

  it("describes multiline paste content", () => {
    expect(pastedTextLineCount("one\r\ntwo\nthree")).toBe(3);
    expect(pastedTextPreview("\n  First useful line  \nSecond")).toBe(
      "First useful line",
    );
  });
});
