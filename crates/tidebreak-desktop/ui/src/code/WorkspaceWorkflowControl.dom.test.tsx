// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../api/client";
import type {
  CodeCheckLogsSnapshot,
  CodeWorkspacePrSnapshot,
} from "../api/types";
import { useCodeUiStore } from "./CodeUiStore";
import type { CodeWorkspacePrResource } from "./useCodeWorkspacePr";
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
});

afterEach(cleanup);

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
