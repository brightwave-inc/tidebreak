// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import type { ApiClient } from "../api/client";
import type { CodeWorkspaceSnapshot, PullRequestDigest } from "../api/types";
import { CodeInspector, inspectorTurnLabel } from "./CodeInspector";
import { useCodeUiStore } from "./CodeUiStore";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";

afterEach(() => {
  cleanup();
  useCodeUpdatesStore.getState().reset();
  useCodeUiStore.setState({
    inspectorScope: null,
    pendingComposerPrompt: null,
  });
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
  | "listCodeWorkspaceTree"
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
    getCodeWorkspaceDiff: vi.fn().mockResolvedValue({
      diff: "",
      truncated: false,
      stat: { files: 0, insertions: 0, deletions: 0, truncated: false },
    }),
    listCodeWorkspaceFiles: vi.fn().mockResolvedValue({
      files: [],
      truncated: false,
      stat: { files: 0, insertions: 0, deletions: 0, truncated: false },
    }),
    listCodeWorkspaceTree: vi.fn().mockResolvedValue({
      paths: [],
      truncated: false,
    }),
    getCodeWorkspacePr: vi.fn().mockResolvedValue(null),
  };
}

it("keeps the PR action bar visible on Files and Source control", async () => {
  render(
    <CodeInspector
      client={makeClient() as never}
      workspaceId="ws-1"
      workspace={{ ...WORKSPACE, pr: PR } as never}
      contentRevision={0}
    />,
  );

  const bar = screen.getByTestId("pr-action-bar");
  expect(bar).toHaveTextContent("#41");
  expect(bar).toHaveTextContent("Draft");
  expect(bar).toHaveTextContent("2 checks");
  expect(screen.getByRole("button", { name: "Merge" })).toBeInTheDocument();

  await userEvent.setup().click(screen.getByRole("tab", { name: "Files" }));
  expect(screen.getByTestId("pr-action-bar")).toBeInTheDocument();
  await userEvent.setup().click(
    screen.getByRole("tab", { name: "Source control" }),
  );
  expect(screen.getByTestId("pr-action-bar")).toBeInTheDocument();

  await userEvent.setup().click(screen.getByRole("button", { name: "Merge" }));
  expect(useCodeUiStore.getState().pendingComposerPrompt).toMatch(
    /Merge pull request #41/,
  );
});

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

  await userEvent.setup().click(screen.getByRole("tab", { name: "Pull request" }));
  await screen.findByText("Fix login flow");

  // Draft wins over the open state token, and holds the tab merge buttons.
  // The inspector bar also says Draft; its Merge writes a prompt instead.
  expect(screen.getAllByText("Draft").length).toBeGreaterThan(0);
  expect(screen.getByText("Changes requested")).toBeInTheDocument();
  expect(
    screen.getByRole("button", { name: "Enable auto-merge" }),
  ).toBeDisabled();
  const tabMerge = screen
    .getAllByRole("button", { name: "Merge" })
    .find((button) => button.hasAttribute("disabled"));
  expect(tabMerge).toBeDefined();

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

it("exposes a tablist and passes the scoped turn into files and source", async () => {
  const client = makeClient();
  useCodeUiStore.setState({
    inspectorScope: { turnId: "turn-1", label: "Turn 4" },
  });
  render(
    <CodeInspector
      client={client as never}
      workspaceId="ws-1"
      workspace={WORKSPACE}
      contentRevision={0}
    />,
  );

  expect(screen.getByRole("tablist")).toBeInTheDocument();
  expect(screen.getByRole("tab", { name: "Source control" })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  // Icon-only tabs, so each carries its own name; the panel it opens is
  // linked back to it.
  const source = screen.getByRole("tab", { name: "Source control" });
  const panel = screen.getByRole("tabpanel");
  expect(source).toHaveAttribute("aria-controls", panel.id);
  expect(panel).toHaveAttribute("aria-labelledby", source.id);
  expect(
    screen.getByRole("button", { name: "Clear Turn 4 scope" }),
  ).toBeInTheDocument();

  await waitFor(() =>
    expect(client.getCodeWorkspaceDiff).toHaveBeenCalledWith("ws-1", {
      turn: "turn-1",
      file: undefined,
    }),
  );

  await userEvent.setup().click(screen.getByRole("tab", { name: "Files" }));
  await waitFor(() =>
    expect(client.listCodeWorkspaceTree).toHaveBeenCalledWith("ws-1", {
      limit: 5000,
    }),
  );
});

it("gives the active inspector tab a selected fill idle tabs do not share", async () => {
  render(
    <CodeInspector
      client={makeClient() as never}
      workspaceId="ws-1"
      workspace={WORKSPACE}
      contentRevision={0}
    />,
  );

  const files = screen.getByRole("tab", { name: "Files" });
  const source = screen.getByRole("tab", { name: "Source control" });
  const pr = screen.getByRole("tab", { name: "Pull request" });

  expect(files).toHaveAttribute("data-state", "active");
  expect(files).toHaveClass("bg-foreground/10");
  expect(source).toHaveAttribute("data-state", "inactive");
  expect(pr).toHaveAttribute("data-state", "inactive");
  expect(source).not.toHaveClass("bg-foreground/10");
  expect(pr).not.toHaveClass("bg-foreground/10");

  await userEvent.setup().click(source);

  expect(source).toHaveAttribute("data-state", "active");
  expect(source).toHaveClass("bg-foreground/10");
  expect(files).not.toHaveClass("bg-foreground/10");
  expect(pr).not.toHaveClass("bg-foreground/10");

  // Radix drives the arrows; this pins that the inspector still gets them.
  source.focus();
  await userEvent.setup().keyboard("{ArrowRight}");
  expect(pr).toHaveAttribute("data-state", "active");
  expect(pr).toHaveFocus();
});

it("labels a turn by its ordinal among user items", () => {
  expect(
    inspectorTurnLabel(
      [
        { kind: "user", id: "u1", turnId: "t-a", text: "one", createdAt: "" },
        { kind: "user", id: "u2", turnId: "t-b", text: "two", createdAt: "" },
      ],
      "t-b",
    ),
  ).toBe("Turn 2");
});
