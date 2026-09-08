import type {
  CodeDeliveryPullRequestDetail,
  CodeDeliveryPullRequestFile,
  CodeDeliveryPullRequestSummary,
} from "@/api/types";
import { deliveryPullRequestDetails, deliveryPullRequests } from "../fixtures";

// These fixtures extend the shared delivery data. Every action stays in React state.
export const reviewFiles: CodeDeliveryPullRequestFile[] = [
  {
    path: "src/code/PullRequestDetail.tsx",
    status: "modified",
    additions: 3,
    deletions: 2,
    patch: [
      "@@ -341,5 +341,6 @@ function PullRequestDetail() {",
      "   useEffect(() => {",
      '-    setDraftComment("");',
      '+    setDraftComment(drafts[prId] ?? "");',
      '     setTab("conversation");',
      "-  }, [initialDetail, summary.id]);",
      "+  }, [summary.id]);",
      "+  // Refresh data without resetting the active review.",
      " }",
    ].join("\n"),
  },
  {
    path: "src/code/delivery/usePullRequestQuery.ts",
    status: "modified",
    additions: 2,
    deletions: 1,
    patch: [
      "@@ -246,3 +246,4 @@ catch (error) {",
      '   setError("Could not refresh pull requests.");',
      "-  setItems([]);",
      "+  // Keep the last successful result visible.",
      '+  setRefreshState("failed");',
      " }",
    ].join("\n"),
  },
  {
    path: "src/code/reviewDrafts.test.tsx",
    status: "added",
    additions: 5,
    deletions: 0,
    patch: [
      "@@ -0,0 +1,5 @@",
      '+it("keeps an unsent review draft", () => {',
      '+  enterDraft("Please preserve this comment.");',
      "+  refreshSamePullRequest();",
      '+  expectDraft("Please preserve this comment.");',
      "+});",
    ].join("\n"),
  },
];

export const reviewPr: CodeDeliveryPullRequestSummary = {
  ...deliveryPullRequests[0]!,
  title: "Keep review drafts during refresh",
  head_branch: "thet/review-drafts",
  comment_count: 2,
  checks: [
    {
      name: "desktop / UI tests",
      bucket: "fail",
      detail: "Failed · 38 seconds",
      workflow_run_id: 4401,
    },
    {
      name: "desktop / typecheck",
      bucket: "pass",
      detail: "Passed · 21 seconds",
    },
    {
      name: "desktop / formatting",
      bucket: "pass",
      detail: "Passed · 7 seconds",
    },
  ],
};
export const readyPr: CodeDeliveryPullRequestSummary = {
  ...deliveryPullRequests[1]!,
  title: "Restore workspace navigation",
};
export const inboxPrs: CodeDeliveryPullRequestSummary[] = [
  reviewPr,
  {
    ...reviewPr,
    id: "storybook/lazy-files#2250",
    number: 2250,
    title: "Load PR files on demand",
    head_branch: "devon/lazy-pr-files",
    author: "devon",
    checks: [{ name: "desktop / tests", bucket: "pass" }],
  },
  readyPr,
];
export const reviewDetail: CodeDeliveryPullRequestDetail = {
  ...deliveryPullRequestDetails[2251]!,
  summary: reviewPr,
  files: reviewFiles,
  changed_files: 3,
  additions: 10,
  deletions: 3,
  commits: 2,
  body: "Preserve comment drafts, the active file, and the selected tab while PR data refreshes.",
};
export const reviewComment = {
  ...reviewDetail.comments[1]!,
  author: "devon",
  path: reviewFiles[0]!.path,
  line: 345,
  body: "Keep the active Files tab when a refresh returns. A fresh detail object does not mean a different PR.",
};
export const failedLog = [
  "FAIL  preserves active file after refresh",
  "",
  "Expected: PullRequestDetail.tsx stays selected",
  "Received: Conversation tab is selected",
  "",
  "reviewDrafts.test.tsx:42",
].join("\n");
