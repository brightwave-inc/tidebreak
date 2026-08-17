import { describe, expect, it } from "vitest";

import { codeRepoIdFromPath, isCodeRoute, shellShortcutMode } from "./routes";

describe("code routes", () => {
  it("reads mode and repo from the path the reader is on", () => {
    // Mode-scoped shortcuts hang off these answers, so a path family read
    // wrongly here sends Cmd+N to the other half of the app — or locks the
    // new-workspace dialog to something that is not a repo.
    expect(isCodeRoute("/code")).toBe(true);
    expect(isCodeRoute("/code/w/ws-1")).toBe(true);
    expect(isCodeRoute("/codex")).toBe(false);
    expect(shellShortcutMode("/c/chat-1")).toBe("chat");
    expect(shellShortcutMode("/code/r/repo-1")).toBe("code");

    expect(codeRepoIdFromPath("/code/r/repo-1")).toBe("repo-1");
    expect(codeRepoIdFromPath("/code/w/ws-1")).toBeUndefined();
    expect(codeRepoIdFromPath("/code")).toBeUndefined();
  });
});
