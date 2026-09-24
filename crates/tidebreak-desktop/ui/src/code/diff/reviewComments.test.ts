import { describe, expect, it } from "vitest";

import {
  commentLinesLabel,
  messageWithReviewComments,
  reviewCommentsFromSent,
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

const PREAMBLE =
  "The person reviewing your changes left these comments on a diff. Each comment names a file and the diff it was written on: the working tree against its base branch, or the changes one turn made. It quotes its lines with diff markers (+ added, - removed) and then gives the comment. Line numbers are from the diff as the reviewer last saw it, so if the file has changed since, find the lines by their quote. A comment marked outdated quotes code that has changed since it was written. Address every comment.";

describe("messageWithReviewComments", () => {
  it("adds one block the model reads: file, diff, lines, quoted lines, comment", () => {
    expect(messageWithReviewComments("Fix these.", [comment()])).toBe(
      [
        "Fix these.",
        "",
        "<review_comments>",
        PREAMBLE,
        "",
        '<comment path="src/queue.ts" diff="working tree" lines="22-23" old_lines="22">',
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

  it("names a turn's diff for the conversation that knows it, and says when it does not", () => {
    const onTurn = comment({ turnId: "turn-b" });
    const named = messageWithReviewComments("", [onTurn], {
      turnName: (turnId) => (turnId === "turn-b" ? "turn 3" : null),
    });
    expect(named).toContain('diff="turn 3" lines="22-23"');
    expect(messageWithReviewComments("", [onTurn])).toContain(
      'diff="an earlier turn"',
    );
  });

  it("says when the quote stops short, and when the code has changed", () => {
    const message = messageWithReviewComments("", [
      comment({
        unquoted: 300,
        span: { lines: "22-322", oldLines: "22" },
        outdated: true,
      }),
    ]);
    expect(message).toContain(
      '<comment path="src/queue.ts" diff="working tree" lines="22-322" old_lines="22" quote="first 3 of 303 lines" outdated="true">',
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
          diff: "working tree",
          lines: "22-23",
          oldLines: "22",
          quote: [" ", "-const MAX = 10;", "+const MAX = 20;"],
          unquoted: 0,
          outdated: false,
          body: "Why double it?",
        },
        {
          path: 'odd "name" <b>.ts',
          diff: "working tree",
          lines: null,
          oldLines: "4",
          quote: ["-gone();"],
          unquoted: 0,
          outdated: false,
          body: "Was this still used?\nCheck the callers.",
        },
      ],
    });
  });

  it("keeps a path with a line break inside its tag", () => {
    const path = "docs/odd\nname.md";
    const message = messageWithReviewComments("", [comment({ path })]);
    expect(message).toContain('path="docs/odd&#10;name.md"');
    expect(splitReviewComments(message).comments[0]?.path).toBe(path);
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

describe("reviewCommentsFromSent", () => {
  it("makes the sent comments pending again, numbered from their spans", () => {
    const message = messageWithReviewComments(
      "Queued.",
      [
        comment({
          turnId: "turn-b",
          lines: [
            { kind: "context", oldNo: 40, newNo: 41, text: "keep();" },
            { kind: "del", oldNo: 41, newNo: null, text: "a();" },
            { kind: "context", oldNo: 42, newNo: 42, text: "between();" },
            { kind: "del", oldNo: 43, newNo: null, text: "b();" },
            { kind: "add", oldNo: null, newNo: 43, text: "c();" },
          ],
          outdated: true,
        }),
      ],
      { turnName: () => "turn 3" },
    );
    const [restored] = reviewCommentsFromSent(
      splitReviewComments(message).comments,
      {
        turnFor: (diff) => (diff === "turn 3" ? "turn-b" : null),
        newId: () => "restored-1",
        now: () => "2026-09-24T12:00:00.000Z",
      },
    );
    expect(restored).toEqual({
      id: "restored-1",
      author: { kind: "person" },
      path: "src/queue.ts",
      turnId: "turn-b",
      lines: [
        { kind: "context", oldNo: null, newNo: 41, text: "keep();" },
        { kind: "del", oldNo: 41, newNo: null, text: "a();" },
        { kind: "context", oldNo: 42, newNo: 42, text: "between();" },
        { kind: "del", oldNo: 43, newNo: null, text: "b();" },
        { kind: "add", oldNo: null, newNo: 43, text: "c();" },
      ],
      outdated: true,
      body: "Why double it?",
      createdAt: "2026-09-24T12:00:00.000Z",
    });
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
