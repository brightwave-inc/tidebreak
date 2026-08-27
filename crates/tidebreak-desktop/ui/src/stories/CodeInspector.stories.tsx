import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, within } from "storybook/test";

import type { ApiClient } from "@/api/client";
import type {
  CodeWorkspacePrSnapshot,
  CodeWorkspaceSnapshot,
  PullRequestDigest,
} from "@/api/types";
import { CodeInspector, type InspectorTab } from "@/code/CodeInspector";
import type { CodeWorkspacePrResource } from "@/code/useCodeWorkspacePr";

type InspectorScenario =
  | "ready"
  | "merge-ready"
  | "merge-queued"
  | "multi-pr"
  | "truncated"
  | "empty"
  | "loading"
  | "failure";

const workspace: CodeWorkspaceSnapshot = {
  id: "ws-inspector-story",
  repo_id: "repo-tidebreak",
  title: "Reconsider the workspace pane system",
  worktree_path: "/Users/sam/tidebreak/worktrees/ui-pane-redesign",
  branch_name: "thet/ui-pane-redesign",
  base_ref: "main",
  status: "active",
  created_at: "2026-08-20T13:00:00.000Z",
};

const pullRequest: PullRequestDigest = {
  number: 2248,
  url: "https://github.com/example/tidebreak/pull/2248",
  state: "open",
  title: "Rework the code workspace around persistent conversation",
  draft: false,
  head_branch: "thet/ui-pane-redesign",
  base_branch: "main",
  review_decision: "approved",
  mergeable: "mergeable",
  merge_state_status: "clean",
  checks_summary: "8 passing, 1 pending",
  checks: [
    { name: "desktop / focused tests", bucket: "pass" },
    { name: "desktop / storybook", bucket: "pass" },
    { name: "workspace / integration", bucket: "pending" },
  ],
};

const workspacePullRequestFacts = [
  {
    host: "github.com",
    repo_owner: "example",
    repo_name: "tidebreak",
    number: 2248,
    url: "https://github.com/example/tidebreak/pull/2248",
    title: "Rework the code workspace around persistent conversation",
    state: "open",
    draft: false,
    author: "sam",
    head_branch: "thet/ui-pane-redesign",
    base_branch: "main",
    head_sha: "6cf15e2a",
    relation: "authored" as const,
    created_at: "2026-08-20T13:10:00.000Z",
    updated_at: "2026-08-20T15:40:00.000Z",
    last_seen_at: "2026-08-20T15:41:00.000Z",
  },
  {
    host: "github.com",
    repo_owner: "example",
    repo_name: "tidebreak",
    number: 2244,
    url: "https://github.com/example/tidebreak/pull/2244",
    title: "Retire the legacy pane",
    state: "merged",
    draft: false,
    author: "sam",
    head_branch: "thet/retire-legacy-pane",
    base_branch: "main",
    relation: "authored" as const,
    created_at: "2026-08-19T09:00:00.000Z",
    updated_at: "2026-08-20T10:00:00.000Z",
    merged_at: "2026-08-20T10:00:00.000Z",
    last_seen_at: "2026-08-20T15:41:00.000Z",
  },
  {
    host: "github.com",
    repo_owner: "example",
    repo_name: "design-tokens",
    number: 87,
    url: "https://github.com/example/design-tokens/pull/87",
    title: "Pane rhythm spacing tokens",
    state: "open",
    draft: true,
    author: "sam",
    head_branch: "thet/pane-rhythm",
    base_branch: "main",
    relation: "contributed" as const,
    created_at: "2026-08-20T14:00:00.000Z",
    updated_at: "2026-08-20T15:00:00.000Z",
    last_seen_at: "2026-08-20T15:41:00.000Z",
  },
];

const prSnapshot: CodeWorkspacePrSnapshot = {
  dirty: true,
  unpushed: false,
  ahead: 2,
  has_upstream: true,
  suggested_commit_message: "Redesign the code workspace panes",
  pr: pullRequest,
  gh_found: true,
  gh_authenticated: true,
  remediation: "",
};

const changedFiles = {
  files: [
    {
      path: "crates/tidebreak-desktop/ui/.storybook/preview.tsx",
      kind: "modified" as const,
      insertions: 8,
      deletions: 1,
    },
    {
      path: "crates/tidebreak-desktop/ui/src/code/CodeWorkspacePage.tsx",
      kind: "modified" as const,
      insertions: 96,
      deletions: 61,
    },
    {
      path: "crates/tidebreak-desktop/ui/src/code/WorkspaceCard.tsx",
      kind: "modified" as const,
      insertions: 142,
      deletions: 301,
    },
    {
      path: "crates/tidebreak-desktop/ui/src/code/editorDrag.ts",
      kind: "added" as const,
      insertions: 57,
      deletions: 0,
    },
    {
      path: "crates/tidebreak-desktop/ui/src/code/LegacyPane.tsx",
      kind: "deleted" as const,
      insertions: 0,
      deletions: 88,
    },
    {
      path: "crates/tidebreak-desktop/ui/src/stories/CodeInspector.stories.tsx",
      previous_path:
        "crates/tidebreak-desktop/ui/src/stories/SourceControl.stories.tsx",
      kind: "renamed" as const,
      insertions: 24,
      deletions: 11,
    },
  ],
  truncated: false,
  stat: { files: 6, insertions: 351, deletions: 462, truncated: false },
};

const truncatedChangedFiles = {
  ...changedFiles,
  truncated: true,
  stat: { files: 82, insertions: 1920, deletions: 341, truncated: true },
};

const avatar = (initials: string, color: string) =>
  `data:image/svg+xml,${encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64"><rect width="64" height="64" rx="32" fill="${color}"/><text x="32" y="39" text-anchor="middle" font-family="system-ui" font-size="22" font-weight="700" fill="white">${initials}</text></svg>`)}`;

function pending<T>(): Promise<T> {
  return new Promise(() => {});
}

function inspectorClient(scenario: InspectorScenario): ApiClient {
  const fail = () =>
    Promise.reject(new Error("The workspace reader is offline."));
  const loadTree =
    scenario === "loading"
      ? () => pending<{ paths: string[]; truncated: boolean }>()
      : scenario === "failure"
        ? fail
        : async () => ({
            paths:
              scenario === "empty"
                ? []
                : [
                    "README.md",
                    "package.json",
                    "pnpm-lock.yaml",
                    "crates/tidebreak-desktop/ui/src/code/CodeWorkspacePage.tsx",
                    "crates/tidebreak-desktop/ui/src/code/WorkspaceCard.tsx",
                    "crates/tidebreak-desktop/ui/src/code/editorDrag.ts",
                    "crates/tidebreak-desktop/ui/src/styles.css",
                    "crates/tidebreak-desktop/ui/public/tidebreak.png",
                    "crates/tidebreak-desktop/ui/src/stories/CodeInspector.stories.tsx",
                    "docs/decisions/0052-subagents.md",
                  ],
            truncated: false,
          });

  return {
    listCodeWorkspaceTree: loadTree,
    searchCodeWorkspace: async () => ({ matches: [], truncated: false }),
    listCodeWorkspaceFiles:
      scenario === "loading"
        ? () => pending<typeof changedFiles>()
        : scenario === "failure"
          ? fail
          : async () =>
              scenario === "empty"
                ? {
                    files: [],
                    truncated: false,
                    stat: {
                      files: 0,
                      insertions: 0,
                      deletions: 0,
                      truncated: false,
                    },
                  }
                : scenario === "truncated"
                  ? truncatedChangedFiles
                  : changedFiles,
    getCodeWorkspacePr: async () => prSnapshot,
    getCodeWorkspacePullRequests: async () => ({
      items:
        scenario === "multi-pr"
          ? workspacePullRequestFacts
          : scenario === "empty"
            ? []
            : workspacePullRequestFacts.slice(0, 1),
      fetched_at: "2026-08-20T15:41:00.000Z",
    }),
    refreshCodeWorkspacePr: async () => prSnapshot,
    commitCodeWorkspace: async () => ({
      sha: "6cf15e2",
      message: prSnapshot.suggested_commit_message,
      stat: changedFiles.stat,
    }),
    pushCodeWorkspace: async () => ({
      branch: workspace.branch_name,
      remote: "origin",
    }),
    createCodePullRequest: async () => prSnapshot,
    mergeCodePr: async () => prSnapshot,
    getCodePrComments: async () => ({
      number: pullRequest.number,
      comments: [
        {
          id: "review-1",
          kind: "review",
          author: "mara",
          avatar_url: avatar("MA", "#635bff"),
          url: "https://github.com/example/tidebreak/pull/2248#pullrequestreview-1",
          review_state: "approved",
          body: "The conversation-first hierarchy feels much calmer :sparkles:\n\n- The pane rhythm reads clearly\n- **Keep** the quick PR status visible",
          created_at: "2026-08-20T14:04:00.000Z",
        },
        {
          id: "inline-1",
          kind: "inline",
          author: "devon",
          avatar_url: avatar("DE", "#0f766e"),
          url: "https://github.com/example/tidebreak/pull/2248#discussion_r1",
          body: "Keep this drop target large enough to acquire while dragging. :eyes:\n\n`min-height: 48px` felt right in testing.",
          path: "src/code/CodeWorkspacePage.tsx",
          line: 839,
          created_at: "2026-08-20T14:12:00.000Z",
        },
      ],
    }),
  } as unknown as ApiClient;
}

function resourceFor(
  snapshot: CodeWorkspacePrSnapshot,
): CodeWorkspacePrResource {
  return {
    data: snapshot,
    error: null,
    refreshing: false,
    refresh: async () => {},
    adopt: () => {},
    busy: null,
    mutationError: null,
    setMutationError: () => {},
    refreshFromHost: async () => snapshot,
    runMutation: async (_mutation, operation) => operation(),
  };
}

function InspectorStory({
  tab,
  scenario = "ready",
}: {
  tab: InspectorTab;
  scenario?: InspectorScenario;
}) {
  const client = inspectorClient(scenario);
  const storyPr =
    scenario === "merge-ready"
      ? {
          ...pullRequest,
          checks_summary: "9 passing",
          checks: pullRequest.checks?.map((check) => ({
            ...check,
            bucket: "pass" as const,
          })),
        }
      : scenario === "merge-queued"
        ? {
            ...pullRequest,
            auto_merge_enabled: true,
            in_merge_queue: true,
          }
        : pullRequest;
  const storyWorkspace =
    scenario === "empty" ? workspace : { ...workspace, pr: storyPr };
  const storySnapshot = { ...prSnapshot, pr: storyPr };

  return (
    <div className="h-[720px] min-w-0 overflow-hidden rounded-xl border border-border-subtle bg-background shadow-sm">
      <CodeInspector
        key={`${tab}:${scenario}`}
        client={client}
        workspaceId={workspace.id}
        workspace={storyWorkspace}
        contentRevision={0}
        prResource={resourceFor(
          scenario === "empty"
            ? { ...prSnapshot, dirty: false, ahead: 0, pr: undefined }
            : storySnapshot,
        )}
        initialTab={tab}
        onOpenFile={fn()}
        onOpenDiff={fn()}
        onClose={fn()}
      />
    </div>
  );
}

const meta = {
  title: "Code/Inspector",
  component: InspectorStory,
  args: { tab: "files", scenario: "ready" },
  decorators: [
    (Story) => (
      <div className="mx-auto w-full max-w-[390px]">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof InspectorStory>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Worktree navigation stays a compact, searchable optional pane. */
export const Files: Story = {};

/** The tab count and changed-file index share the same live workspace read. */
export const Changes: Story = {
  args: { tab: "source" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(await canvas.findByLabelText("6 changed files")).toBeVisible();
  },
};

/** Checks, merge state, and review discussion are one readable review surface. */
export const Review: Story = {
  args: { tab: "pr" },
};

/** The same compact merge card when GitHub says the PR can land now. */
export const ReviewReadyToMerge: Story = {
  args: { tab: "pr", scenario: "merge-ready" },
};

/** The review tab and detail header use GitHub's orange queue mark. */
export const ReviewMergeQueued: Story = {
  args: { tab: "pr", scenario: "merge-queued" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText("Queued")).toBeVisible();
    await expect(canvas.getByText("In merge queue")).toBeVisible();
    await expect(canvas.queryByText("Auto-merge is enabled")).toBeNull();
  },
};

/**
 * Several attributed pull requests (decision 62): the set lists above the
 * panel, the primary stays live, and a selected row shows its stored
 * snapshot.
 */
export const ReviewPullRequestSet: Story = {
  args: { tab: "pr", scenario: "multi-pr" },
};

export const ChangesLoading: Story = {
  args: { tab: "source", scenario: "loading" },
};

export const ChangesEmpty: Story = {
  args: { tab: "source", scenario: "empty" },
};

export const ChangesFailure: Story = {
  args: { tab: "source", scenario: "failure" },
};

export const ChangesTruncated: Story = {
  args: { tab: "source", scenario: "truncated" },
};

export const Empty: Story = {
  args: { tab: "files", scenario: "empty" },
};

export const Loading: Story = {
  args: { tab: "files", scenario: "loading" },
};

export const Failure: Story = {
  args: { tab: "files", scenario: "failure" },
};

export const Compact: Story = {
  args: { tab: "pr" },
  decorators: [
    (Story) => (
      <div className="mx-auto w-[330px]">
        <Story />
      </div>
    ),
  ],
};
