// @vitest-environment jsdom
import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { HttpError, type ApiClient } from "../api/client";
import type {
  CodeCheckLogsSnapshot,
  CodeWorkspacePrSnapshot,
} from "../api/types";
import { useCodeUiStore } from "./CodeUiStore";
import type { CodeWorkspacePrResource } from "./useCodeWorkspacePr";
import {
  resetWorkflowPromptStore,
  useWorkflowPromptStore,
} from "./workflowPrompts";
import { WorkspaceWorkflowControl } from "./WorkspaceWorkflowControl";

vi.mock("sonner", () => ({
  toast: {
    message: vi.fn(),
    success: vi.fn(),
    error: vi.fn(),
    warning: vi.fn(),
  },
}));

const failingPr: CodeWorkspacePrSnapshot = {
  dirty: false,
  unpushed: false,
  ahead: 0,
  has_upstream: true,
  suggested_commit_message: "Fix login",
  gh_found: true,
  gh_authenticated: true,
  remediation: "",
  pr: {
    number: 184,
    url: "https://github.com/example/app/pull/184",
    state: "open",
    title: "Fix login",
    checks_summary: "7 passing, 1 failing",
    checks: [
      { name: "desktop test", bucket: "pass" },
      {
        name: "clippy",
        bucket: "fail",
        detail: "exit 101",
        url: "https://github.com/example/app/actions/runs/7/job/9",
      },
    ],
  },
};

function resource(): CodeWorkspacePrResource {
  return {
    data: failingPr,
    error: null,
    refreshing: false,
    refresh: async () => {},
    adopt: () => {},
    busy: null,
    mutationError: null,
    setMutationError: () => {},
    refreshFromHost: async () => undefined,
    runMutation: async () => undefined,
  };
}

function renderControl(
  writeCodeCheckLogs: ApiClient["writeCodeCheckLogs"],
): void {
  render(
    <WorkspaceWorkflowControl
      client={
        {
          pushCodeWorkspace: vi.fn(),
          createCodePullRequest: vi.fn(),
          markCodePrReady: vi.fn(),
          mergeCodePr: vi.fn(),
          startCodeWatch: vi.fn(),
          stopCodeWatch: vi.fn(),
          writeCodeCheckLogs,
        } as unknown as ApiClient
      }
      workspaceId="ws-1"
      branchName="tidebreak/fix-login"
      baseRef="main"
      resource={resource()}
      onOpenSourceControl={vi.fn()}
    />,
  );
}

const snapshot = (
  logs: CodeCheckLogsSnapshot["logs"],
): CodeCheckLogsSnapshot => ({ logs, errors: [] });

beforeEach(() => {
  useCodeUiStore.setState({
    pendingComposerPrompt: null,
    composerActionScope: null,
  });
  resetWorkflowPromptStore();
});

afterEach(cleanup);

const dirtyLocal: CodeWorkspacePrSnapshot = {
  dirty: true,
  unpushed: false,
  ahead: 0,
  has_upstream: true,
  suggested_commit_message: "improve login flow",
  gh_found: true,
  gh_authenticated: true,
  remediation: "",
};

function renderDirtyControl() {
  const dirtyResource: CodeWorkspacePrResource = {
    ...resource(),
    data: dirtyLocal,
  };
  render(
    <WorkspaceWorkflowControl
      client={
        {
          pushCodeWorkspace: vi.fn(),
          createCodePullRequest: vi.fn(),
          markCodePrReady: vi.fn(),
          mergeCodePr: vi.fn(),
          startCodeWatch: vi.fn(),
          stopCodeWatch: vi.fn(),
          writeCodeCheckLogs: vi.fn(),
        } as unknown as ApiClient
      }
      workspaceId="ws-1"
      branchName="tidebreak/fix-login"
      baseRef="main"
      resource={dirtyResource}
      onOpenSourceControl={vi.fn()}
    />,
  );
}

describe("Create PR", () => {
  it("submits the prompt instead of leaving it in the composer", async () => {
    renderDirtyControl();
    await userEvent.click(screen.getByRole("button", { name: "Create PR" }));
    const pending = useCodeUiStore.getState().pendingComposerPrompt;
    expect(pending?.submit).toBe(true);
    expect(pending?.scope).toBe("ws-1");
    expect(pending?.text).toContain("open a pull request against `main`");
    expect(pending?.text).toContain("Do not merge.");
  });

  it("sends a prompt edited in settings", async () => {
    useWorkflowPromptStore
      .getState()
      .setPrompt("compose_pr", "Ship this branch to {base}.");
    renderDirtyControl();
    await userEvent.click(screen.getByRole("button", { name: "Create PR" }));
    expect(useCodeUiStore.getState().pendingComposerPrompt?.text).toBe(
      "Ship this branch to `main`.",
    );
  });
});

describe("Fix CI", () => {
  it("downloads the failing job logs and names them in the prompt", async () => {
    const write = vi.fn(async () =>
      snapshot([
        {
          check: "clippy",
          path: "/data/code/private/ws-1/ci-logs/clippy-9.log",
          byte_len: 4096,
          truncated: false,
          url: "https://github.com/example/app/actions/runs/7/job/9",
        },
      ]),
    );
    renderControl(write);

    await userEvent.click(screen.getByRole("button", { name: /fix ci/i }));

    await waitFor(() =>
      expect(useCodeUiStore.getState().pendingComposerPrompt).not.toBeNull(),
    );
    expect(write).toHaveBeenCalledWith("ws-1");
    const pending = useCodeUiStore.getState().pendingComposerPrompt;
    expect(pending?.submit).toBe(true);
    expect(pending?.text).toContain("/ci-logs/clippy-9.log");
    expect(pending?.text).toContain("already downloaded");
  });

  /**
   * The download is a host read the reader waits on. Without a busy state the
   * button reads as dead, and a second press would start a second fetch.
   */
  it("holds the button through the download", async () => {
    let release: ((value: CodeCheckLogsSnapshot) => void) | undefined;
    const write = vi.fn(
      () =>
        new Promise<CodeCheckLogsSnapshot>((resolve) => {
          release = resolve;
        }),
    );
    renderControl(write);

    const button = screen.getByRole("button", { name: /fix ci/i });
    await userEvent.click(button);
    const reading = await screen.findByText("Reading logs…");
    expect(reading).toBeTruthy();
    expect(screen.getByText("Reading logs…").closest("button")).toBeDisabled();

    release?.(snapshot([]));
    await waitFor(() =>
      expect(useCodeUiStore.getState().pendingComposerPrompt).not.toBeNull(),
    );
    expect(write).toHaveBeenCalledTimes(1);
  });

  /** A GitHub outage must not disable the action. */
  it("still sends the prompt when the download fails", async () => {
    renderControl(
      vi.fn(async () => {
        throw new Error("gh is signed out");
      }),
    );

    await userEvent.click(screen.getByRole("button", { name: /fix ci/i }));

    await waitFor(() =>
      expect(useCodeUiStore.getState().pendingComposerPrompt).not.toBeNull(),
    );
    const pending = useCodeUiStore.getState().pendingComposerPrompt;
    expect(pending?.text).toContain("Inspect the latest failing CI logs");
    expect(pending?.text).not.toContain("already downloaded");
  });
});

const readyPr: CodeWorkspacePrSnapshot = {
  ...failingPr,
  pr: {
    number: 184,
    url: "https://github.com/example/app/pull/184",
    state: "open",
    title: "Fix login",
    head_branch: "tidebreak/fix-login",
    base_branch: "main",
    head_sha: "abcdef1234567890",
    mergeable: "mergeable",
    merge_state_status: "clean",
    checks: [{ name: "ci", bucket: "pass" }],
  },
};

function renderMergeControl(error?: HttpError) {
  const mergeCodePr = vi.fn(async () => {
    if (error) throw error;
    return {
      ...readyPr,
      pr: { ...readyPr.pr!, state: "merged", merged: true },
    };
  });
  const refresh = vi.fn(async () => {});
  const adopt = vi.fn();
  const setMutationError = vi.fn();
  const mergeResource: CodeWorkspacePrResource = {
    data: readyPr,
    error: null,
    refreshing: false,
    refresh,
    adopt,
    busy: null,
    mutationError: null,
    setMutationError,
    refreshFromHost: async () => undefined,
    runMutation: async (_mutation, operation) => operation(),
  };
  render(
    <WorkspaceWorkflowControl
      client={
        {
          pushCodeWorkspace: vi.fn(),
          createCodePullRequest: vi.fn(),
          markCodePrReady: vi.fn(),
          mergeCodePr,
          startCodeWatch: vi.fn(),
          stopCodeWatch: vi.fn(),
          writeCodeCheckLogs: vi.fn(),
        } as unknown as ApiClient
      }
      workspaceId="ws-1"
      branchName="tidebreak/fix-login"
      baseRef="main"
      resource={mergeResource}
      onOpenSourceControl={vi.fn()}
    />,
  );
  return { mergeCodePr, refresh, adopt, setMutationError };
}

describe("Merge", () => {
  it("submits the pull request and head shown in the confirmation", async () => {
    const { mergeCodePr, adopt } = renderMergeControl();
    await userEvent.click(screen.getByRole("button", { name: "Merge" }));
    const dialog = await screen.findByRole("alertdialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Merge" }),
    );

    await waitFor(() =>
      expect(mergeCodePr).toHaveBeenCalledWith("ws-1", {
        target: {
          repository: {
            host: "github.com",
            owner: "example",
            name: "app",
          },
          number: 184,
        },
        expected_head_sha: "abcdef1234567890",
        method: "squash",
        auto: false,
      }),
    );
    expect(adopt).toHaveBeenCalledOnce();
  });

  it.each([
    "workspace_dirty",
    "workspace_unpushed",
    "pr_head_changed",
    "pr_target_changed",
  ])("keeps refresh available after %s", async (kind) => {
    const { refresh, setMutationError } = renderMergeControl(
      new HttpError(409, `409: ${kind}`, kind),
    );
    await userEvent.click(screen.getByRole("button", { name: "Merge" }));
    const dialog = await screen.findByRole("alertdialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Merge" }),
    );

    const refreshButton = await screen.findByRole("button", {
      name: "Refresh workspace status",
    });
    expect(setMutationError).toHaveBeenCalledWith(
      expect.stringContaining("Refresh workspace status"),
    );
    await userEvent.click(refreshButton);
    expect(refresh).toHaveBeenCalledOnce();
  });
});
