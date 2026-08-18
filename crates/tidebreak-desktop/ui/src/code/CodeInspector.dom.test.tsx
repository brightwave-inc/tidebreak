// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

import type { ApiClient } from "../api/client";
import type { CodeWorkspaceSnapshot, PullRequestDigest } from "../api/types";
import { CodeInspector } from "./CodeInspector";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";

afterEach(() => {
  cleanup();
  useCodeUpdatesStore.getState().reset();
  vi.clearAllMocks();
});

const WORKSPACE: CodeWorkspaceSnapshot = {
  id: "ws-1",
  repo_id: "repo-1",
  title: "Fix login",
  branch_name: "tidebreak/fix-login",
  status: "active",
  created_at: "2026-08-01T00:00:00Z",
} as never;

const PR: PullRequestDigest = {
  number: 41,
  state: "open",
  title: "Fix login flow",
  url: "https://github.com/acme/app/pull/41",
  draft: true,
  review_decision: "changes_requested",
  head_branch: "tidebreak/fix-login",
  base_branch: "main",
  checks: [
    { name: "ci / rust", bucket: "pass" },
    { name: "ci / ui", bucket: "pending" },
  ],
} as never;

function makeClient(): Pick<
  ApiClient,
  | "getCodePrComments"
  | "refreshCodeWorkspacePr"
  | "mergeCodePr"
  | "getCodeWorkspaceDiff"
  | "listCodeWorkspaceFiles"
  | "getCodeWorkspacePr"
> {
  return {
    getCodePrComments: vi.fn().mockResolvedValue({
      number: 41,
      comments: [
        {
          kind: "inline",
          author: "reviewer",
          body: "Rename this.",
          path: "src/login.rs",
          line: 12,
          created_at: "2026-08-02T10:00:00Z",
        },
      ],
    }),
    refreshCodeWorkspacePr: vi.fn(),
    mergeCodePr: vi.fn(),
    getCodeWorkspaceDiff: vi.fn().mockResolvedValue({ diff: "" }),
    listCodeWorkspaceFiles: vi.fn().mockResolvedValue({ files: [] }),
    getCodeWorkspacePr: vi.fn().mockResolvedValue(null),
  };
}

it("shows PR state, checks, comments, and holds merge for a draft", async () => {
  const client = makeClient();
  render(
    <CodeInspector
      client={client as never}
      workspaceId="ws-1"
      workspace={{ ...WORKSPACE, pr: PR } as never}
      contentRevision={0}
    />,
  );

  screen.getByRole("button", { name: "Pull request" }).click();
  await screen.findByText("Fix login flow");

  // Draft wins over the open state token, and holds the merge buttons.
  expect(screen.getByText("Draft")).toBeInTheDocument();
  expect(screen.getByText("Changes requested")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Merge" })).toBeDisabled();
  expect(
    screen.getByRole("button", { name: "Enable auto-merge" }),
  ).toBeDisabled();

  // Checks render individually with their buckets counted.
  expect(screen.getByText("1 passing")).toBeInTheDocument();
  expect(screen.getByText("1 pending")).toBeInTheDocument();
  expect(screen.getByText("ci / rust")).toBeInTheDocument();

  // Comments load on open, with the inline anchor visible.
  await waitFor(() =>
    expect(screen.getByText("Rename this.")).toBeInTheDocument(),
  );
  expect(screen.getByText("src/login.rs:12")).toBeInTheDocument();
  expect(client.getCodePrComments).toHaveBeenCalledWith("ws-1");
});
