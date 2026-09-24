import type { CodeReviewFinding, CodeReviewSnapshot } from "../../api/types";
import { groupUnifiedDiff } from "../unifiedDiff";
import { anchorRows } from "./commentAnchor";
import { diffRows, isCodeRow, type DiffRow } from "./diffModel";
import {
  commentLinesLabel,
  type ReviewComment,
  type ReviewCommentAuthor,
} from "./reviewComments";

/**
 * A finished review's findings, as pending comments on the diff it read.
 *
 * Each finding names lines of the new side of the diff. They are anchored the
 * way a person's comment is (`commentAnchor.ts`): the comment keeps the lines
 * it quotes and the code around them, from the diff the reviewer read, so it
 * finds its lines again in the diff as it is now, however far the agent has
 * moved on, or says it is outdated.
 *
 * Findings arrive proposed: shown in the diff, but not going with the next
 * message until the person keeps or edits them. Findings on lines the diff
 * does not show, and an answer that could not be read as findings, become
 * one comment on the changes as a whole, so nothing the reviewer said is
 * lost and nothing is pinned to a line it was not about.
 */

/** The id a finding's comment keeps: importing a review twice adds it once. */
export function findingCommentId(reviewId: string, index: number): string {
  return `review:${reviewId}:${index}`;
}

/** The id of a review's comment on the changes as a whole. */
export function summaryCommentId(reviewId: string): string {
  return `review:${reviewId}:summary`;
}

/** The rows a finding's new-side lines cover, inside one hunk, or null. */
export function findingRows(
  rows: readonly DiffRow[],
  finding: Pick<CodeReviewFinding, "start_line" | "end_line">,
): { start: number; end: number } | null {
  let start = -1;
  let end = -1;
  const seen = new Set<number>();
  rows.forEach((row, index) => {
    if (!isCodeRow(row) || row.kind === "del" || row.newNo === null) return;
    if (row.newNo < finding.start_line || row.newNo > finding.end_line) return;
    if (start < 0) start = index;
    end = index;
    seen.add(row.newNo);
  });
  if (start < 0 || rows[start]!.hunk !== rows[end]!.hunk) return null;
  // Every line it names is in the diff, or it is not on the diff at all.
  return seen.size === finding.end_line - finding.start_line + 1
    ? { start, end }
    : null;
}

function reviewerOf(
  review: CodeReviewSnapshot,
): Extract<ReviewCommentAuthor, { kind: "reviewer" }> {
  return {
    kind: "reviewer",
    engine: review.harness,
    ...(review.model ? { model: review.model } : {}),
    reviewId: review.id,
  };
}

function linesOf(finding: CodeReviewFinding): string {
  return commentLinesLabel({
    lines:
      finding.start_line === finding.end_line
        ? String(finding.start_line)
        : `${finding.start_line}-${finding.end_line}`,
    oldLines: null,
  }).toLowerCase();
}

/** The whole-change comment: the unread answer, or the findings off the diff. */
function summaryBody(
  rawText: string | undefined,
  offDiff: readonly CodeReviewFinding[],
): { title: string; body: string } | null {
  if (rawText !== undefined && rawText.trim()) {
    return { title: "The review's answer", body: rawText.trim() };
  }
  if (offDiff.length === 0) return null;
  return {
    title:
      offDiff.length === 1
        ? "A finding on lines outside the diff"
        : `${offDiff.length} findings on lines outside the diff`,
    body: offDiff
      .map(
        (finding) =>
          `- ${finding.path}, ${linesOf(finding)} (${finding.severity}): ${finding.title}\n  ${finding.explanation.replace(/\n/g, "\n  ")}`,
      )
      .join("\n"),
  };
}

/**
 * The pending comments a completed review adds: one per finding on the
 * diff, and at most one about the changes as a whole. Empty for a review
 * that did not complete.
 */
export function commentsFromReview(
  review: CodeReviewSnapshot,
  options: { now?: () => string } = {},
): ReviewComment[] {
  const result = review.result;
  if (review.status !== "completed" || !result) return [];
  const author = reviewerOf(review);
  const createdAt =
    review.finished_at ?? options.now?.() ?? new Date().toISOString();
  const turn = review.turn_id ? { turnId: review.turn_id } : {};
  const groups = new Map(
    groupUnifiedDiff(result.diff).map((group) => [group.path, group]),
  );
  const rowsByPath = new Map<string, DiffRow[]>();
  const comments: ReviewComment[] = [];
  const offDiff: CodeReviewFinding[] = [];
  result.findings.forEach((finding, index) => {
    const group = groups.get(finding.path);
    if (!group) {
      offDiff.push(finding);
      return;
    }
    let rows = rowsByPath.get(finding.path);
    if (!rows) {
      rows = diffRows(group);
      rowsByPath.set(finding.path, rows);
    }
    const covered = findingRows(rows, finding);
    if (!covered) {
      offDiff.push(finding);
      return;
    }
    const { span, ...anchor } = anchorRows(rows, covered.start, covered.end);
    comments.push({
      id: findingCommentId(review.id, index),
      author,
      path: finding.path,
      ...turn,
      ...anchor,
      ...(anchor.unquoted ? { span } : {}),
      body: finding.explanation,
      createdAt,
      severity: finding.severity,
      title: finding.title,
      proposed: true,
    });
  });
  offDiff.push(...result.unplaced);
  const summary = summaryBody(result.raw_text, offDiff);
  if (summary) {
    comments.push({
      id: summaryCommentId(review.id),
      author,
      path: "",
      ...turn,
      lines: [],
      body: summary.body,
      createdAt,
      title: summary.title,
      proposed: true,
      general: true,
    });
  }
  return comments;
}
