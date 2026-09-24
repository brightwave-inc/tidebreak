import type { CodeReviewFinding, CodeReviewSnapshot } from "../../api/types";
import { groupUnifiedDiff } from "../unifiedDiff";
import { anchorRows } from "./commentAnchor";
import { diffRows, isCodeRow, type DiffRow } from "./diffModel";
import type { ReviewComment, ReviewCommentAuthor } from "./reviewComments";

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
 * message until the person keeps or edits them. A finding on lines the diff
 * does not show becomes a comment of its own above the files, naming its
 * file and lines, with the same severity and title as one in the diff. An
 * answer that could not be read as findings becomes one comment on the
 * changes as a whole, so nothing the reviewer said is lost and nothing is
 * pinned to a line it was not about.
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

/** A finding's lines as a span, "40" or "40-41". */
function findingSpan(finding: CodeReviewFinding): string {
  return finding.start_line === finding.end_line
    ? String(finding.start_line)
    : `${finding.start_line}-${finding.end_line}`;
}

/**
 * The pending comments a completed review adds: one per finding, on its
 * lines when the diff shows them and above the files when it does not, and
 * one for an answer that was not findings. Empty for a review that did not
 * complete.
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
  const onDiff: ReviewComment[] = [];
  const offDiff: ReviewComment[] = [];

  function offTheDiff(finding: CodeReviewFinding, index: number) {
    offDiff.push({
      id: findingCommentId(review.id, index),
      author,
      path: finding.path,
      ...turn,
      lines: [],
      span: { lines: findingSpan(finding), oldLines: null },
      body: finding.explanation,
      createdAt,
      severity: finding.severity,
      title: finding.title,
      proposed: true,
      general: true,
    });
  }

  result.findings.forEach((finding, index) => {
    const group = groups.get(finding.path);
    if (!group) {
      offTheDiff(finding, index);
      return;
    }
    let rows = rowsByPath.get(finding.path);
    if (!rows) {
      rows = diffRows(group);
      rowsByPath.set(finding.path, rows);
    }
    const covered = findingRows(rows, finding);
    if (!covered) {
      offTheDiff(finding, index);
      return;
    }
    const { span, ...anchor } = anchorRows(rows, covered.start, covered.end);
    onDiff.push({
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
  result.unplaced.forEach((finding, index) =>
    offTheDiff(finding, result.findings.length + index),
  );
  const answer =
    result.raw_text !== undefined && result.raw_text.trim()
      ? [
          {
            id: summaryCommentId(review.id),
            author,
            path: "",
            ...turn,
            lines: [],
            body: result.raw_text.trim(),
            createdAt,
            title: "The review's answer",
            proposed: true as const,
            general: true as const,
          },
        ]
      : [];
  return [...onDiff, ...answer, ...offDiff];
}
