// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { CodeRepoTrustSnapshot, HarnessKind } from "@/api/types";
import {
  confirmRepositoryTrust,
  RepositoryTrustSheetHost,
  useRepositoryTrustStore,
} from "./RepositoryTrustStore";

afterEach(() => {
  cleanup();
  useRepositoryTrustStore.setState({ queue: [] });
});

const workspace = {
  id: "workspace-1",
  worktree_path: "/Users/mara/Tidebreak/workspaces/tidebreak/fix-1",
  repo_display_name: "brightwave-inc/tidebreak",
};

const hooks: CodeRepoTrustSnapshot = {
  repo_id: "repo-1",
  trust: "undecided",
  files: [
    {
      path: ".claude/settings.json",
      engines: ["claude_code"],
      effects: [{ kind: "hooks", count: 2 }],
    },
    {
      path: ".mcp.json",
      engines: ["claude_code"],
      effects: [{ kind: "mcp_servers", count: 1 }],
    },
  ],
};

function client(snapshot: CodeRepoTrustSnapshot) {
  return {
    getCodeWorkspaceTrust: vi.fn(async () => snapshot),
    setCodeRepoTrust: vi.fn(async (_id: string, trusted: boolean) => ({
      ...snapshot,
      trust: trusted ? ("trusted" as const) : ("untrusted" as const),
    })),
  };
}

function ask(api: ReturnType<typeof client>, harness: HarnessKind) {
  return confirmRepositoryTrust({ client: api, workspace, harness });
}

describe("confirmRepositoryTrust", () => {
  it("lists each file and records trust before the session starts", async () => {
    const user = userEvent.setup();
    const api = client(hooks);
    render(<RepositoryTrustSheetHost />);

    const outcome = ask(api, "claude_code");
    expect(
      await screen.findByRole("alertdialog", {
        name: "Trust this repository?",
      }),
    ).toBeInTheDocument();
    expect(screen.getByText(".claude/settings.json")).toBeInTheDocument();
    expect(screen.getByText(/2 hooks/)).toBeInTheDocument();
    expect(screen.getByText(/1 MCP server/)).toBeInTheDocument();

    await user.click(
      screen.getByRole("button", { name: "Trust this repository" }),
    );

    await expect(outcome).resolves.toBe("trusted");
    expect(api.setCodeRepoTrust).toHaveBeenCalledWith("repo-1", true);
    await waitFor(() =>
      expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument(),
    );
  });

  it("records the choice to continue without the settings", async () => {
    const user = userEvent.setup();
    const api = client(hooks);
    render(<RepositoryTrustSheetHost />);

    const outcome = ask(api, "claude_code");
    await user.click(
      await screen.findByRole("button", {
        name: "Continue without its settings",
      }),
    );

    await expect(outcome).resolves.toBe("untrusted");
    expect(api.setCodeRepoTrust).toHaveBeenCalledWith("repo-1", false);
  });

  it("keeps the sheet open with the reason when trust cannot be saved", async () => {
    const user = userEvent.setup();
    const api = client(hooks);
    api.setCodeRepoTrust.mockRejectedValueOnce(new Error("git config failed"));
    render(<RepositoryTrustSheetHost />);

    const outcome = ask(api, "claude_code");
    await user.click(
      await screen.findByRole("button", { name: "Trust this repository" }),
    );

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "git config failed",
    );
    await user.click(
      screen.getByRole("button", { name: "Trust this repository" }),
    );
    await expect(outcome).resolves.toBe("trusted");
  });

  it("starts at once when there is nothing to ask", async () => {
    render(<RepositoryTrustSheetHost />);
    const cases: [CodeRepoTrustSnapshot, HarnessKind][] = [
      [{ ...hooks, files: [] }, "claude_code"],
      [{ ...hooks, trust: "trusted" }, "claude_code"],
      [{ ...hooks, trust: "untrusted" }, "claude_code"],
      // Nothing here loads for Codex, so its session has nothing to ask.
      [hooks, "codex"],
    ];
    for (const [snapshot, harness] of cases) {
      const api = client(snapshot);
      await expect(ask(api, harness)).resolves.toBeNull();
      expect(api.setCodeRepoTrust).not.toHaveBeenCalled();
    }
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
  });

  it("does not ask about a sandbox workspace or one the reader does not own", async () => {
    const api = client(hooks);
    await expect(
      confirmRepositoryTrust({
        client: api,
        workspace: { ...workspace, worktree_path: "remote:workspace-1" },
        harness: "claude_code",
      }),
    ).resolves.toBeNull();
    await expect(
      confirmRepositoryTrust({
        client: api,
        workspace: { ...workspace, is_owner: false },
        harness: "claude_code",
      }),
    ).resolves.toBeNull();
    expect(api.getCodeWorkspaceTrust).not.toHaveBeenCalled();
  });

  it("starts without asking when the trust read fails", async () => {
    const api = client(hooks);
    api.getCodeWorkspaceTrust.mockRejectedValueOnce(new Error("offline"));
    await expect(ask(api, "claude_code")).resolves.toBeNull();
  });

  it("asks once when two sessions start in one repository", async () => {
    const api = client(hooks);
    const first = ask(api, "claude_code");
    const second = ask(api, "claude_code");
    await waitFor(() =>
      expect(useRepositoryTrustStore.getState().queue).toHaveLength(1),
    );
    const [request] = useRepositoryTrustStore.getState().queue;
    act(() => useRepositoryTrustStore.getState().settle(request.id, "trusted"));
    await expect(first).resolves.toBe("trusted");
    await expect(second).resolves.toBe("trusted");
  });
});
