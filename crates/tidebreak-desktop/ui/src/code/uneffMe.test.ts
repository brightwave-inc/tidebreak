import { describe, expect, it, vi } from "vitest";

import type { CodeRepoSnapshot, CodeWorkspaceSnapshot } from "../api/types";
import {
  DEBUG_JSON_PROMPT_BUDGET,
  debugJsonForPrompt,
  isTidebreakProductRepo,
  startUneffMeWorkspace,
  tidebreakProductRepo,
  uneffMePrompt,
  uneffMeWorkspaceTitle,
} from "./uneffMe";

const APP: CodeRepoSnapshot = {
  id: "repo-app",
  root_path: "/tmp/app",
  display_name: "app",
  default_base_ref: "main",
  branch_prefix: "tidebreak",
  quick_actions: [],
  created_at: "2026-08-15T00:00:00.000Z",
};

const TIDEBREAK: CodeRepoSnapshot = {
  ...APP,
  id: "repo-tb",
  root_path: "/Users/sam/src/tidebreak",
  display_name: "tidebreak",
};

describe("tidebreak product repo", () => {
  it("matches the product checkout, not a worktree folder", () => {
    expect(isTidebreakProductRepo(TIDEBREAK)).toBe(true);
    expect(
      isTidebreakProductRepo({
        ...APP,
        display_name: "notes",
        root_path: "/Users/sam/Tidebreak/workspaces/tidebreak/ember-orchard",
      }),
    ).toBe(false);
    expect(
      isTidebreakProductRepo({
        ...APP,
        display_name: "brightwave-inc/tidebreak",
        root_path: "/tmp/clone",
      }),
    ).toBe(true);
  });

  it("prefers the display name tidebreak when several match", () => {
    const clone: CodeRepoSnapshot = {
      ...TIDEBREAK,
      id: "repo-clone",
      display_name: "tidebreak-src",
      root_path: "/opt/tidebreak",
    };
    expect(tidebreakProductRepo([APP, clone, TIDEBREAK])?.id).toBe("repo-tb");
  });
});

describe("uneff me prompt", () => {
  it("names the source session and includes the debug JSON", () => {
    const prompt = uneffMePrompt({
      sourceTitle: "Fix login",
      sourceBranch: "tidebreak/fix-login",
      sourceRepo: "app",
      sessionId: "sess-1",
      debug: { session: { id: "sess-1" }, turns: [], events: [] },
    });
    expect(prompt).toContain("Fix login");
    expect(prompt).toContain("tidebreak/fix-login");
    expect(prompt).toContain("sess-1");
    expect(prompt).toContain("Open a pull request against main");
    expect(prompt).toContain('"id": "sess-1"');
    expect(prompt).not.toContain("omitted");
  });

  it("drops journal events when the dump is too large", () => {
    const events = Array.from({ length: 80 }, (_, index) => ({
      seq: index,
      blob: "x".repeat(2_000),
    }));
    const packed = debugJsonForPrompt({
      session: { id: "sess-1" },
      turns: [{ id: "turn-1" }],
      events,
    });
    expect(packed.omitted).toBe("events");
    expect(packed.text).toContain('"sess-1"');
    expect(packed.text).not.toContain('"blob"');
    expect(packed.text.length).toBeLessThanOrEqual(DEBUG_JSON_PROMPT_BUDGET);

    const prompt = uneffMePrompt({
      sourceTitle: "Fix login",
      sourceBranch: "tidebreak/fix-login",
      sourceRepo: "app",
      sessionId: "sess-1",
      debug: { session: { id: "sess-1" }, turns: [{ id: "turn-1" }], events },
    });
    expect(prompt).toContain("Journal events were omitted");
  });

  it("truncates a title the way a fresh PR-agent workspace does", () => {
    expect(uneffMeWorkspaceTitle("Fix login")).toBe("Uneff: Fix login");
    expect(uneffMeWorkspaceTitle("x".repeat(80)).length).toBe(60);
    expect(uneffMeWorkspaceTitle("x".repeat(80)).endsWith("…")).toBe(true);
  });
});

describe("startUneffMeWorkspace", () => {
  it("creates a Tidebreak workspace and returns the prompt", async () => {
    const created: CodeWorkspaceSnapshot = {
      id: "ws-fix",
      repo_id: TIDEBREAK.id,
      title: "Uneff: Fix login",
      worktree_path: "/tmp/tidebreak/.worktrees/uneff",
      branch_name: "tidebreak/uneff-fix-login",
      base_ref: "main",
      status: "active",
      created_at: "2026-08-26T00:00:00.000Z",
    };
    const getDebug = vi.fn(async () => ({ session: { id: "sess-1" } }));
    const createWorkspace = vi.fn(async () => created);

    const result = await startUneffMeWorkspace({
      repos: [APP, TIDEBREAK],
      sessionId: "sess-1",
      sourceTitle: "Fix login",
      sourceBranch: "tidebreak/fix-login",
      sourceRepo: "app",
      getDebug,
      createWorkspace,
    });

    expect(getDebug).toHaveBeenCalledWith("sess-1");
    expect(createWorkspace).toHaveBeenCalledWith({
      repo_id: TIDEBREAK.id,
      title: "Uneff: Fix login",
    });
    expect(result.workspace).toEqual(created);
    expect(result.prompt).toContain("sess-1");
  });

  it("refuses to start when Tidebreak is not connected", async () => {
    await expect(
      startUneffMeWorkspace({
        repos: [APP],
        sessionId: "sess-1",
        sourceTitle: "Fix login",
        sourceBranch: "tidebreak/fix-login",
        sourceRepo: "app",
        getDebug: vi.fn(),
        createWorkspace: vi.fn(),
      }),
    ).rejects.toThrow("Add the Tidebreak repository to Code first.");
  });
});
