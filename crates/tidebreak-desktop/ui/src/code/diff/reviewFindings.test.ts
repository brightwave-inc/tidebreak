import { describe, expect, it } from "vitest";

import type { CodeReviewSnapshot } from "../../api/types";
import { groupUnifiedDiff } from "../unifiedDiff";
import { placeComment } from "./commentAnchor";
import { diffRows } from "./diffModel";
import { messageWithReviewComments } from "./reviewComments";
import {
  commentsFromReview,
  findingCommentId,
  summaryCommentId,
} from "./reviewFindings";

/** The diff the reviewer read: `MAX` doubled, and a retry loop added. */
const REVIEWED = [
  "diff --git a/src/queue.ts b/src/queue.ts",
  "--- a/src/queue.ts",
  "+++ b/src/queue.ts",
  "@@ -1,5 +1,7 @@",
  " import { send } from './net';",
  "-const MAX = 10;",
  "+const MAX = 20;",
  " ",
  " export function flush(items: string[]) {",
  "+  while (true) send(items);",
  "+  // never stops",
  " }",
  "",
].join("\n");

function review(
  overrides: Partial<CodeReviewSnapshot> = {},
  result: Partial<NonNullable<CodeReviewSnapshot["result"]>> = {},
): CodeReviewSnapshot {
  return {
    id: "rev-1",
    workspace_id: "ws-1",
    session_id: "sess-1",
    harness: "codex",
    model: "gpt-5.5",
    permission_mode: "plan",
    status: "completed",
    progress: { tool_calls: 3, files_read: 1, refused: 0 },
    started_at: "2026-09-24T10:00:00.000Z",
    finished_at: "2026-09-24T10:02:00.000Z",
    result: {
      findings: [
        {
          path: "src/queue.ts",
          start_line: 5,
          end_line: 6,
          severity: "high",
          title: "The flush loop never stops",
          explanation:
            "`while (true)` sends forever. Stop when the batch is empty.",
        },
      ],
      unplaced: [],
      rejected: 0,
      diff: REVIEWED,
      ...result,
    },
    ...overrides,
  };
}

describe("commentsFromReview", () => {
  it("anchors each finding to the lines it names, as a proposed comment by the reviewer", () => {
    const [comment, ...rest] = commentsFromReview(review());
    expect(rest).toEqual([]);
    expect(comment).toEqual({
      id: findingCommentId("rev-1", 0),
      author: {
        kind: "reviewer",
        engine: "codex",
        model: "gpt-5.5",
        reviewId: "rev-1",
      },
      path: "src/queue.ts",
      lines: [
        {
          kind: "add",
          oldNo: null,
          newNo: 5,
          text: "  while (true) send(items);",
        },
        { kind: "add", oldNo: null, newNo: 6, text: "  // never stops" },
      ],
      context: {
        before: [
          "export function flush(items: string[]) {",
          "",
          "const MAX = 20;",
        ],
        after: ["}"],
      },
      body: "`while (true)` sends forever. Stop when the batch is empty.",
      createdAt: "2026-09-24T10:02:00.000Z",
      severity: "high",
      title: "The flush loop never stops",
      proposed: true,
    });
  });

  it("follows its code when the agent moves it, the way a person's comment does", () => {
    const [comment] = commentsFromReview(review());
    // The agent added two imports above; every number below moved by two.
    const moved = REVIEWED.replace(
      "@@ -1,5 +1,7 @@\n import { send } from './net';",
      "@@ -1,5 +1,9 @@\n import { send } from './net';\n+import { log } from './log';\n+import { retry } from './retry';",
    );
    const rows = diffRows(groupUnifiedDiff(moved)[0]!);
    const placement = placeComment(rows, comment!);
    expect(placement.kind).toBe("placed");
    if (placement.kind !== "placed") return;
    expect(placement.lines.map((line) => line.newNo)).toEqual([7, 8]);

    // And when the quoted code itself changes, it is outdated, not moved.
    const rewritten = REVIEWED.replace(
      "+  while (true) send(items);",
      "+  for (const item of items) send([item]);",
    );
    expect(
      placeComment(diffRows(groupUnifiedDiff(rewritten)[0]!), comment!).kind,
    ).toBe("outdated");
  });

  it("gives each finding the diff does not show its own comment, with its file, lines, severity, and title", () => {
    const comments = commentsFromReview(
      review(
        {},
        {
          findings: [
            {
              path: "src/queue.ts",
              start_line: 40,
              end_line: 41,
              severity: "medium",
              title: "Lines the diff does not show",
              explanation: "The client dropped these from the placed list.",
            },
          ],
          unplaced: [
            {
              path: "src/net.ts",
              start_line: 3,
              end_line: 3,
              severity: "low",
              title: "An unchanged helper",
              explanation: "Nothing calls it any more.",
            },
          ],
        },
      ),
    );
    expect(comments).toEqual([
      expect.objectContaining({
        id: findingCommentId("rev-1", 0),
        author: expect.objectContaining({
          kind: "reviewer",
          engine: "codex",
          reviewId: "rev-1",
        }),
        path: "src/queue.ts",
        lines: [],
        span: { lines: "40-41", oldLines: null },
        general: true,
        proposed: true,
        severity: "medium",
        title: "Lines the diff does not show",
        body: "The client dropped these from the placed list.",
      }),
      expect.objectContaining({
        id: findingCommentId("rev-1", 1),
        path: "src/net.ts",
        span: { lines: "3", oldLines: null },
        general: true,
        severity: "low",
        title: "An unchanged helper",
        body: "Nothing calls it any more.",
      }),
    ]);
  });

  it("keeps an answer that was not findings as one comment, word for word", () => {
    const comments = commentsFromReview(
      review(
        {},
        {
          findings: [],
          raw_text: "Looks fine, but the retry loop worries me.",
        },
      ),
    );
    expect(comments).toEqual([
      expect.objectContaining({
        id: summaryCommentId("rev-1"),
        general: true,
        title: "The review's answer",
        body: "Looks fine, but the retry loop worries me.",
      }),
    ]);
  });

  it("adds nothing for a review that did not complete, and names a turn's diff", () => {
    expect(commentsFromReview(review({ status: "failed" }))).toEqual([]);
    expect(commentsFromReview(review({ status: "running" }))).toEqual([]);
    const onTurn = commentsFromReview(review({ turn_id: "turn-3" }));
    expect(onTurn[0]?.turnId).toBe("turn-3");
  });

  it("sends a kept finding to the agent with its reviewer, severity, and title", () => {
    const [comment] = commentsFromReview(review());
    const { proposed: _proposed, ...kept } = comment!;
    const message = messageWithReviewComments("", [kept]);
    expect(message).toContain(
      '<comment path="src/queue.ts" diff="working tree" lines="5-6" reviewer="codex" severity="high" title="The flush loop never stops">',
    );
    expect(message).toContain("read-only review of these changes");
  });
});
