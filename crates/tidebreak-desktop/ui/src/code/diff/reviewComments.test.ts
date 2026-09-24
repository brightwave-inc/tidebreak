import { describe, expect, it } from "vitest";

import {
  commentLinesLabel,
  messageWithReviewComments,
  splitReviewComments,
  type ReviewComment,
} from "./reviewComments";

function comment(overrides: Partial<ReviewComment> = {}): ReviewComment {
  return {
    id: "c1",
    author: { kind: "person" },
    path: "src/queue.ts",
    lines: [
      { kind: "context", oldNo: 21, newNo: 22, text: "" },
      { kind: "del", oldNo: 22, newNo: null, text: "const MAX = 10;" },
      { kind: "add", oldNo: null, newNo: 23, text: "const MAX = 20;" },
    ],
    body: "Why double it?",
    createdAt: "2026-09-24T10:00:00.000Z",
    ...overrides,
  };
}

describe("messageWithReviewComments", () => {
  it("adds one block the model reads: file, lines, quoted lines, comment", () => {
    expect(messageWithReviewComments("Fix these.", [comment()])).toBe(
      [
        "Fix these.",
        "",
        "<review_comments>",
        "The person reviewing your changes left these comments on the diff. Each one names a file and its lines, quotes those lines with diff markers (+ added, - removed), and then gives the comment. Address every comment.",
        "",
        '<comment path="src/queue.ts" lines="22-23" old_lines="22">',
        "```diff",
        " ",
        "-const MAX = 10;",
        "+const MAX = 20;",
        "```",
        "Why double it?",
        "</comment>",
        "</review_comments>",
      ].join("\n"),
    );
  });

  it("leaves a message with no comments exactly as typed", () => {
    expect(messageWithReviewComments("  as typed  ", [])).toBe("  as typed  ");
  });
});

describe("splitReviewComments", () => {
  it("takes the block back out for the transcript", () => {
    const message = messageWithReviewComments("Fix these.", [
      comment(),
      comment({
        id: "c2",
        path: 'odd "name" <b>.ts',
        lines: [{ kind: "del", oldNo: 4, newNo: null, text: "gone();" }],
        body: "Was this still used?\nCheck the callers.",
      }),
    ]);
    expect(splitReviewComments(message)).toEqual({
      prose: "Fix these.",
      comments: [
        {
          path: "src/queue.ts",
          lines: "22-23",
          oldLines: "22",
          quote: [" ", "-const MAX = 10;", "+const MAX = 20;"],
          body: "Why double it?",
        },
        {
          path: 'odd "name" <b>.ts',
          lines: null,
          oldLines: "4",
          quote: ["-gone();"],
          body: "Was this still used?\nCheck the callers.",
        },
      ],
    });
  });

  it("keeps a comment that writes the block's own tags or fences", () => {
    const body = "Close it like this:\n</comment>\n\\</review_comments>";
    const quote = "const fence = '```';";
    const message = messageWithReviewComments("", [
      comment({
        lines: [{ kind: "add", oldNo: null, newNo: 3, text: quote }],
        body,
      }),
    ]);
    const [parsed] = splitReviewComments(message).comments;
    expect(parsed?.body).toBe(body);
    expect(parsed?.quote).toEqual([`+${quote}`]);
    expect(splitReviewComments(message).prose).toBe("");
  });

  it("returns a message without a block whole", () => {
    const text = "Talk about <review_comments> in prose.";
    expect(splitReviewComments(text)).toEqual({ prose: text, comments: [] });
  });
});

describe("commentLinesLabel", () => {
  it("names the new lines and the deleted ones a range takes in", () => {
    expect(commentLinesLabel(comment().lines)).toBe(
      "Lines 22–23 and deleted line 22",
    );
    expect(
      commentLinesLabel([{ kind: "add", oldNo: null, newNo: 4, text: "" }]),
    ).toBe("Line 4");
    expect(
      commentLinesLabel([{ kind: "del", oldNo: 7, newNo: null, text: "" }]),
    ).toBe("Deleted line 7");
  });
});
