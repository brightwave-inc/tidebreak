import { describe, expect, it } from "vitest";

import { orderPullRequestComments } from "./pullRequestPresentation";

describe("orderPullRequestComments", () => {
  const comments = [
    { kind: "issue" as const, id: "a", created_at: "2026-08-20T10:00:00Z" },
    { kind: "issue" as const, id: "b", created_at: "2026-08-20T15:00:00Z" },
    { kind: "issue" as const, id: "undated" },
    { kind: "issue" as const, id: "c", created_at: "2026-08-20T12:00:00Z" },
  ].map((comment) => ({ ...comment, body: comment.id }));

  it("puts the latest comment on top by default order", () => {
    expect(
      orderPullRequestComments(comments, "newest").map((item) => item.id),
    ).toEqual(["b", "c", "a", "undated"]);
  });

  it("restores host chronology on oldest-first", () => {
    expect(
      orderPullRequestComments(comments, "oldest").map((item) => item.id),
    ).toEqual(["undated", "a", "c", "b"]);
  });

  it("keeps the input untouched", () => {
    const before = comments.map((item) => item.id);
    orderPullRequestComments(comments, "newest");
    expect(comments.map((item) => item.id)).toEqual(before);
  });
});
