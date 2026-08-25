// @vitest-environment jsdom
import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import type { ApiClient } from "../api/client";
import type {
  CodeWorkspacePrSnapshot,
  CodeWorkspaceSnapshot,
  PullRequestDigest,
} from "../api/types";
import { CodeInspector, inspectorTurnLabel } from "./CodeInspector";
import { useCodeUiStore } from "./CodeUiStore";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";
import type { CodeWorkspacePrResource } from "./useCodeWorkspacePr";

afterEach(() => {
  cleanup();
  window.localStorage.clear();
  useCodeUpdatesStore.getState().reset();
  useCodeUiStore.setState({
    inspectorScope: null,
    pendingComposerPrompt: null,
    composerActionScope: null,
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

const OPEN_PR: PullRequestDigest = {
  number: 41,
  state: "open",
  title: "Fix login flow",
  url: "https://github.com/acme/app/pull/41",
  draft: false,
  head_branch: "tidebreak/fix-login",
  base_branch: "main",
};

const CLEAN_PR_SNAPSHOT: Omit<CodeWorkspacePrSnapshot, "pr"> = {
  dirty: false,
  unpushed: false,
  ahead: 0,
  has_upstream: true,
  suggested_commit_message: "",
  gh_found: true,
  gh_authenticated: true,
  remediation: "",
};

function makeClient(): Pick<
  ApiClient,
  | "getCodePrComments"
  | "refreshCodeWorkspacePr"
  | "mergeCodePr"
  | "getCodeWorkspaceDiff"
  | "listCodeWorkspaceFiles"
  | "listCodeWorkspaceTree"
  | "getCodeWorkspacePr"
  | "getCodeWorkspacePullRequests"
> {
  return {
    getCodePrComments: vi.fn().mockResolvedValue({
      number: 41,
      comments: [
        {
          id: "99",
          kind: "inline",
          author: "reviewer",
          avatar_url:
            "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg'/%3E",
          url: "https://github.com/acme/app/pull/41#discussion_r99",
          body: "**Rename** this :rocket:",
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
    getCodeWorkspacePullRequests: vi.fn().mockResolvedValue({
      items: [],
      fetched_at: "2026-08-22T12:00:00Z",
    }),
  };
}

it("leaves quick PR actions to the workspace header", async () => {
  render(
    <CodeInspector
      client={makeClient() as never}
      workspaceId="ws-1"
      workspace={{ ...WORKSPACE, pr: PR } as never}
      contentRevision={0}
    />,
  );

  expect(screen.queryByTestId("pr-action-bar")).not.toBeInTheDocument();

  await userEvent.setup().click(screen.getByRole("tab", { name: "Files" }));
  expect(screen.queryByTestId("pr-action-bar")).not.toBeInTheDocument();
  await userEvent
    .setup()
    .click(screen.getByRole("tab", { name: "Source control" }));
  expect(screen.queryByTestId("pr-action-bar")).not.toBeInTheDocument();
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

  await userEvent
    .setup()
    .click(screen.getByRole("tab", { name: "Pull request" }));
  await screen.findByText("Fix login flow");

  // Draft wins over the open state token and offers no impossible merge action.
  expect(screen.getAllByText("Draft").length).toBeGreaterThan(0);
  expect(screen.getByText("Changes requested")).toBeInTheDocument();
  expect(
    screen.queryByRole("button", { name: "Enable auto-merge" }),
  ).not.toBeInTheDocument();
  expect(
    screen.queryByRole("button", { name: "Squash and merge" }),
  ).not.toBeInTheDocument();
  expect(
    screen.queryByRole("combobox", { name: "Merge method" }),
  ).not.toBeInTheDocument();

  // Checks render individually with their buckets counted.
  expect(screen.getByText("1 passing")).toBeInTheDocument();
  expect(screen.getByText("1 pending")).toBeInTheDocument();
  expect(screen.getByText("ci / rust")).toBeInTheDocument();

  // Comments load on open, with the inline anchor visible.
  await waitFor(() =>
    expect(
      screen.getByText("Rename", { selector: "strong" }),
    ).toBeInTheDocument(),
  );
  expect(screen.getByText(/this 🚀/)).toBeInTheDocument();
  expect(screen.getByText("src/login.rs:12")).toBeInTheDocument();
  expect(client.getCodePrComments).toHaveBeenCalledWith("ws-1");
});

it.each([
  [
    "unknown mergeability",
    OPEN_PR,
    "GitHub is still determining mergeability. Merge stays unavailable until the pull request is explicitly ready.",
    true,
  ],
  [
    "conflicts",
    { ...OPEN_PR, mergeable: "conflicting", merge_state_status: "dirty" },
    "Resolve the merge conflicts before merging directly.",
    false,
  ],
  [
    "a behind branch",
    { ...OPEN_PR, mergeable: "mergeable", merge_state_status: "behind" },
    "Update the branch from its base before merging directly.",
    true,
  ],
  [
    "a required review approval",
    {
      ...OPEN_PR,
      mergeable: "mergeable",
      merge_state_status: "blocked",
      review_decision: "review_required",
    },
    "The pull request needs a review approval before merging directly.",
    true,
  ],
  [
    "a blocked repository requirement",
    {
      ...OPEN_PR,
      mergeable: "mergeable",
      merge_state_status: "blocked",
    },
    "A repository requirement is still blocking a direct merge.",
    true,
  ],
  [
    "requested changes",
    {
      ...OPEN_PR,
      mergeable: "mergeable",
      merge_state_status: "clean",
      review_decision: "changes_requested",
    },
    "Address the requested changes before merging directly.",
    true,
  ],
  [
    "failing checks",
    {
      ...OPEN_PR,
      mergeable: "mergeable",
      merge_state_status: "clean",
      checks: [{ name: "ci / ui", bucket: "fail" }],
    },
    "Fix the failing checks before merging directly.",
    true,
  ],
  [
    "pending checks",
    {
      ...OPEN_PR,
      mergeable: "mergeable",
      merge_state_status: "clean",
      checks: [{ name: "ci / ui", bucket: "pending" }],
    },
    "Wait for the pending checks before merging directly.",
    true,
  ],
  [
    "merge queue membership",
    {
      ...OPEN_PR,
      mergeable: "mergeable",
      merge_state_status: "clean",
      in_merge_queue: true,
    },
    "This pull request is already waiting in the merge queue.",
    false,
  ],
] as const)(
  "holds direct merge for %s and explains why",
  async (_, pr, copy, autoMergeEnabled) => {
    render(
      <CodeInspector
        client={makeClient() as never}
        workspaceId="ws-1"
        workspace={{ ...WORKSPACE, pr } as never}
        contentRevision={0}
      />,
    );

    await userEvent
      .setup()
      .click(screen.getByRole("tab", { name: "Pull request" }));

    expect(
      screen.queryByRole("button", { name: "Squash and merge" }),
    ).not.toBeInTheDocument();
    const autoMerge = screen.queryByRole("button", {
      name: "Enable auto-merge",
    });
    if (autoMergeEnabled) expect(autoMerge).toBeEnabled();
    else expect(autoMerge).not.toBeInTheDocument();
    expect(screen.getByText(copy)).toBeInTheDocument();
  },
);

it("only enables direct merge for an affirmatively ready PR", async () => {
  render(
    <CodeInspector
      client={makeClient() as never}
      workspaceId="ws-1"
      workspace={
        {
          ...WORKSPACE,
          pr: {
            ...OPEN_PR,
            mergeable: "mergeable",
            merge_state_status: "clean",
            checks: [{ name: "ci / ui", bucket: "pass" }],
          },
        } as never
      }
      contentRevision={0}
    />,
  );

  await userEvent
    .setup()
    .click(screen.getByRole("tab", { name: "Pull request" }));

  expect(
    screen.getByRole("button", { name: "Squash and merge" }),
  ).toBeEnabled();
  expect(
    screen.getByRole("button", { name: "Enable auto-merge instead" }),
  ).toBeEnabled();
});

it("routes manual refresh through the shared serialized PR resource", async () => {
  const client = makeClient();
  const refreshed = {
    ...CLEAN_PR_SNAPSHOT,
    pr: {
      ...OPEN_PR,
      mergeable: "mergeable",
      merge_state_status: "clean",
    },
  };
  const refreshFromHost = vi.fn(async () => refreshed);
  const resource = {
    data: { ...CLEAN_PR_SNAPSHOT, pr: OPEN_PR },
    error: null,
    refreshing: false,
    refresh: vi.fn(async () => undefined),
    adopt: vi.fn(),
    busy: null,
    mutationError: null,
    setMutationError: vi.fn(),
    refreshFromHost,
    runMutation: vi.fn(),
  } as unknown as CodeWorkspacePrResource;
  render(
    <CodeInspector
      client={client as never}
      workspaceId="ws-1"
      workspace={{ ...WORKSPACE, pr: OPEN_PR } as never}
      contentRevision={0}
      prResource={resource}
    />,
  );

  await userEvent
    .setup()
    .click(screen.getByRole("tab", { name: "Pull request" }));
  await userEvent
    .setup()
    .click(screen.getByRole("button", { name: "Refresh pull request" }));

  await waitFor(() => expect(refreshFromHost).toHaveBeenCalledOnce());
  expect(client.refreshCodeWorkspacePr).not.toHaveBeenCalled();
});

it("shows skipped checks as neutral and hides stale review state after merge", async () => {
  const merged: PullRequestDigest = {
    ...PR,
    state: "merged",
    merged: true,
    draft: false,
    review_decision: "review_required",
    checks: [
      { name: "ci / rust", bucket: "pass" },
      { name: "release draft", bucket: "skipped", detail: "skipping" },
    ],
  } as never;
  render(
    <CodeInspector
      client={makeClient() as never}
      workspaceId="ws-1"
      workspace={{ ...WORKSPACE, pr: merged } as never}
      contentRevision={0}
    />,
  );

  await userEvent
    .setup()
    .click(screen.getByRole("tab", { name: "Pull request" }));
  expect(screen.getAllByText("Merged").length).toBeGreaterThan(0);
  expect(screen.getByText("1 passing")).toBeInTheDocument();
  expect(screen.getByText("1 skipped")).toBeInTheDocument();
  expect(screen.queryByText("Review required")).not.toBeInTheDocument();
  expect(
    screen.queryByRole("button", { name: "Squash and merge" }),
  ).not.toBeInTheDocument();
});

it("attaches, locally resolves, hides, and restores a rich review comment", async () => {
  const client = makeClient();
  const user = userEvent.setup();
  render(
    <CodeInspector
      client={client as never}
      workspaceId="ws-1"
      workspace={{ ...WORKSPACE, pr: OPEN_PR } as never}
      contentRevision={0}
    />,
  );

  await user.click(screen.getByRole("tab", { name: "Pull request" }));
  await screen.findByText("Rename", { selector: "strong" });
  expect(
    document.querySelector("img[src^='data:image/svg+xml']"),
  ).not.toBeNull();

  await user.click(
    screen.getByRole("button", { name: "Comment actions for reviewer" }),
  );
  await user.click(screen.getByRole("menuitem", { name: "Attach to chat" }));
  expect(useCodeUiStore.getState().pendingComposerPrompt).toMatchObject({
    scope: "ws-1",
    submit: false,
  });
  expect(useCodeUiStore.getState().pendingComposerPrompt?.text).toContain(
    "src/login.rs:12",
  );

  await user.click(
    screen.getByRole("button", { name: "Comment actions for reviewer" }),
  );
  await user.click(
    screen.getByRole("menuitem", { name: "Mark resolved in Tidebreak" }),
  );
  expect(screen.getByText("Resolved here")).toBeInTheDocument();

  await user.click(
    screen.getByRole("button", { name: "Comment actions for reviewer" }),
  );
  await user.click(screen.getByRole("menuitem", { name: "Hide in Tidebreak" }));
  expect(
    screen.queryByText("Rename", { selector: "strong" }),
  ).not.toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Show 1 hidden" }));
  expect(
    await screen.findByText("Rename", { selector: "strong" }),
  ).toBeInTheDocument();
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
  // Visible labels and accessible names stay linked to the panel they open.
  const source = screen.getByRole("tab", { name: "Source control" });
  const panel = screen.getByRole("tabpanel");
  expect(source).toHaveAttribute("aria-controls", panel.id);
  expect(panel).toHaveAttribute("aria-labelledby", source.id);
  expect(
    screen.getByRole("button", { name: "Clear Turn 4 scope" }),
  ).toBeInTheDocument();

  await waitFor(() =>
    expect(client.listCodeWorkspaceFiles).toHaveBeenCalledWith(
      "ws-1",
      "turn-1",
    ),
  );

  await userEvent.setup().click(screen.getByRole("tab", { name: "Files" }));
  await waitFor(() =>
    expect(client.listCodeWorkspaceTree).toHaveBeenCalledWith("ws-1", {
      limit: 5000,
    }),
  );
});

it("keeps patch contents out of Source control and opens them in the center", async () => {
  const client = makeClient();
  vi.mocked(client.listCodeWorkspaceFiles).mockResolvedValue({
    files: [
      {
        path: "src/login.rs",
        kind: "modified",
        insertions: 3,
        deletions: 1,
      },
    ],
    truncated: false,
    stat: { files: 1, insertions: 3, deletions: 1, truncated: false },
  });
  const onOpenDiff = vi.fn();

  render(
    <CodeInspector
      client={client as never}
      workspaceId="ws-1"
      workspace={WORKSPACE}
      contentRevision={0}
      onOpenDiff={onOpenDiff}
    />,
  );

  await userEvent
    .setup()
    .click(screen.getByRole("tab", { name: "Source control" }));
  await userEvent
    .setup()
    .click(screen.getByRole("button", { name: /Modified src\/login\.rs/ }));

  expect(onOpenDiff).toHaveBeenCalledWith("src/login.rs");
  expect(client.listCodeWorkspaceFiles).toHaveBeenCalledWith("ws-1", undefined);
  expect(client.getCodeWorkspaceDiff).not.toHaveBeenCalled();
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
  expect(files).toHaveClass("bg-background");
  expect(source).toHaveAttribute("data-state", "inactive");
  expect(pr).toHaveAttribute("data-state", "inactive");
  expect(source).not.toHaveClass("bg-background");
  expect(pr).not.toHaveClass("bg-background");

  await userEvent.setup().click(source);

  expect(source).toHaveAttribute("data-state", "active");
  expect(source).toHaveClass("bg-background");
  expect(files).not.toHaveClass("bg-background");
  expect(pr).not.toHaveClass("bg-background");

  // Radix drives the arrows; this pins that the inspector still gets them.
  source.focus();
  await userEvent.setup().keyboard("{ArrowRight}");
  expect(pr).toHaveAttribute("data-state", "active");
  expect(pr).toHaveFocus();
});

it("shows the changed-file count in the Changes tab", async () => {
  const client = makeClient();
  vi.mocked(client.listCodeWorkspaceFiles).mockResolvedValue({
    files: [
      {
        path: "src/login.rs",
        kind: "modified",
        insertions: 3,
        deletions: 1,
      },
    ],
    truncated: true,
    stat: { files: 12, insertions: 30, deletions: 10, truncated: true },
  });

  render(
    <CodeInspector
      client={client as never}
      workspaceId="ws-1"
      workspace={WORKSPACE}
      contentRevision={0}
    />,
  );

  const source = screen.getByRole("tab", { name: "Source control" });
  expect(
    await within(source).findByLabelText("12 changed files"),
  ).toHaveTextContent("12");
});

it("closes the review sidebar from its own chrome", async () => {
  const onClose = vi.fn();
  render(
    <CodeInspector
      client={makeClient() as never}
      workspaceId="ws-1"
      workspace={WORKSPACE}
      contentRevision={0}
      onClose={onClose}
    />,
  );

  await userEvent
    .setup()
    .click(screen.getByRole("button", { name: "Close review sidebar" }));

  expect(onClose).toHaveBeenCalledOnce();
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

it("keys the attributed set on full identity, not the number", async () => {
  const collidingFacts = [
    {
      host: "github.com",
      repo_owner: "acme",
      repo_name: "app",
      number: 41,
      url: "https://github.com/acme/app/pull/41",
      title: "Fix login flow",
      state: "open",
      draft: false,
      head_branch: "tidebreak/fix-login",
      base_branch: "main",
      relation: "authored" as const,
      created_at: "2026-08-01T00:00:00Z",
      updated_at: "2026-08-02T00:00:00Z",
      last_seen_at: "2026-08-02T00:00:00Z",
    },
    {
      host: "github.com",
      repo_owner: "acme",
      repo_name: "design-tokens",
      number: 41,
      url: "https://github.com/acme/design-tokens/pull/41",
      title: "Token spacing pass",
      state: "open",
      draft: false,
      head_branch: "tidebreak/tokens",
      base_branch: "main",
      relation: "contributed" as const,
      created_at: "2026-08-01T00:00:00Z",
      updated_at: "2026-08-02T00:00:00Z",
      last_seen_at: "2026-08-02T00:00:00Z",
    },
  ];
  const client = {
    ...makeClient(),
    getCodeWorkspacePullRequests: vi.fn().mockResolvedValue({
      items: collidingFacts,
      fetched_at: "2026-08-02T00:01:00Z",
    }),
  };
  render(
    <CodeInspector
      client={client as never}
      workspaceId="ws-1"
      workspace={{ ...WORKSPACE, pr: OPEN_PR } as never}
      contentRevision={0}
      requestedTab={{ tab: "pr", revision: 1 }}
    />,
  );

  const list = await screen.findByRole("navigation", {
    name: "Pull requests this workspace worked on",
  });
  const rows = await waitFor(() => {
    const buttons = list.querySelectorAll("button");
    expect(buttons).toHaveLength(2);
    return buttons;
  });

  // The live number matches both repositories, so no row may claim to be
  // current: treating either as the primary would collapse two identities.
  expect(list.querySelectorAll("[aria-current='true']")).toHaveLength(0);

  // Selecting the cross-repo row shows that row's stored snapshot — never
  // the live resource its number collides with.
  await userEvent.click(rows[1]);
  const panelTitle = await screen.findByRole("link", {
    name: "Token spacing pass",
  });
  expect(panelTitle.getAttribute("href")).toBe(
    "https://github.com/acme/design-tokens/pull/41",
  );
  const current = list.querySelectorAll("[aria-current='true']");
  expect(current).toHaveLength(1);
  expect(current[0].textContent).toContain("design-tokens");
});
