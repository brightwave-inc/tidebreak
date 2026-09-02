import { describe, expect, it } from "vitest";

import {
  LONG_PASTE_MIN_CHARACTERS,
  messageWithPastedText,
  pastedTextLineCount,
  pastedTextPreview,
  shouldAttachPastedText,
  splitPastedText,
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

  it("splits held paste back out of a sent message", () => {
    const sent = messageWithPastedText("Summarize this", [
      { id: "paste-1", text: "First line\nSecond line" },
      { id: "paste-2", text: '{\n  "a": 1\n}' },
    ]);
    expect(splitPastedText(sent)).toEqual({
      prose: "Summarize this",
      pasted: ["First line\nSecond line", '{\n  "a": 1\n}'],
    });
    expect(splitPastedText("plain message")).toEqual({
      prose: "plain message",
      pasted: [],
    });
  });

  it("keeps a block whole when its body carries its own paste wrappers", () => {
    // Uneff me pastes a debug report whose turns may hold earlier pastes.
    const inner = messageWithPastedText("Summarize this", [
      { id: "paste-1", text: "inner body" },
    ]);
    const report = JSON.stringify({ turns: [{ user_input: inner }] }, null, 2);
    const sent = messageWithPastedText("The report follows.", [
      { id: "report", text: report },
    ]);
    expect(splitPastedText(sent)).toEqual({
      prose: "The report follows.",
      pasted: [report],
    });

    // An opener that never balances runs to the end, not into the prose.
    const unbalanced =
      "Look\n\n<pasted_text>\n<pasted_text>\nstray\n</pasted_text>";
    expect(splitPastedText(unbalanced)).toEqual({
      prose: "Look",
      pasted: ["<pasted_text>\nstray\n</pasted_text>"],
    });
  });
});
