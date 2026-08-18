// @vitest-environment jsdom
import { cleanup, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import { renderWithRouter } from "@/test/router";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { disconnectCodeUpdates, useCodeUpdatesStore } from "./CodeUpdatesStore";
import { CodeSidebar } from "./CodeSidebar";

/**
 * ADR 0030: the code rail must render without initializing chat session
 * stores. This file never imports ChatSessionStore or ChatListStore.
 */

const client = {
  listCodeRepos: vi.fn(async () => [
    {
      id: "repo-1",
      root_path: "/tmp/app",
      display_name: "app",
      default_base_ref: "main",
      branch_prefix: "tidebreak",
      quick_actions: [],
      created_at: "2026-08-15T00:00:00.000Z",
    },
  ]),
  listCodeWorkspaces: vi.fn(async () => [
    {
      id: "ws-1",
      repo_id: "repo-1",
      title: "Fix login",
      worktree_path: "/tmp/app/.worktrees/fix-login",
      branch_name: "tidebreak/fix-login",
      base_ref: "main",
      status: "active" as const,
      created_at: "2026-08-15T00:00:00.000Z",
    },
  ]),
  getHarnessDoctor: vi.fn(async () => ({ harnesses: [] })),
  listCodeHarnessModels: vi.fn(async () => ({
    kind: "claude_code" as const,
    models: [],
  })),
  getCodeCloneDefaults: vi.fn(async () => ({
    gh_found: false,
    gh_remediation: "gh is not installed.",
  })),
  openCodeUpdates: vi.fn(() => {
    return {
      close() {},
      addEventListener() {},
      removeEventListener() {},
    } as unknown as WebSocket;
  }),
};

const app: AppContextValue = {
  client: client as never,
  models: [],
  defaultModelKey: null,
  providers: [],
  refreshCatalog: async () => {},
  refreshChats: async () => {},
  status: "",
  setStatus: () => {},
  newChat: () => {},
  deleteChat: () => {},
  startRename: () => {},
  commitRename: () => {},
  cancelRename: () => {},
  newProject: async () => false,
  deleteProject: () => {},
  startProjectRename: () => {},
  commitProjectRename: () => {},
  cancelProjectRename: () => {},
  newChatInProject: () => {},
  moveChatToProject: () => {},
  updateState: { status: "idle", version: null, error: null, enabled: false },
  updateUpToDate: false,
  checkForUpdate: async () => ({
    status: "idle",
    version: null,
    error: null,
    enabled: false,
  }),
  restartForUpdate: async () => {},
};

afterEach(() => {
  cleanup();
  useCodeCatalogStore.getState().reset();
  disconnectCodeUpdates();
  useCodeUpdatesStore.getState().reset();
});

describe("CodeSidebar", () => {
  it("renders the code rail without chat stores initialized", async () => {
    await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    expect(screen.getByRole("button", { name: "Chat" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Code" })).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: "app" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Fix login" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "New workspace" })).toBeInTheDocument();
  });
});
