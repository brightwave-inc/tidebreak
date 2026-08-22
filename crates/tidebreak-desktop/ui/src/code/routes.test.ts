import { describe, expect, it } from "vitest";

import {
  codeWorkspaceIdFromPath,
  isCodeRoute,
  shellShortcutMode,
} from "./routes";

describe("code routes", () => {
  it("reads mode and workspace from the path the reader is on", () => {
    // Mode-scoped shortcuts hang off these answers, so a path family read
    // wrongly here sends Cmd+N to the other half of the app — or points the
    // terminal and review shortcuts at nothing.
    expect(isCodeRoute("/code")).toBe(true);
    expect(isCodeRoute("/code/w/ws-1")).toBe(true);
    expect(isCodeRoute("/codex")).toBe(false);
    expect(shellShortcutMode("/c/chat-1")).toBe("chat");
    expect(shellShortcutMode("/code/archive")).toBe("code");

    expect(codeWorkspaceIdFromPath("/code/w/ws-1")).toBe("ws-1");
    expect(codeWorkspaceIdFromPath("/code/archive")).toBeUndefined();
    expect(codeWorkspaceIdFromPath("/code")).toBeUndefined();
  });
});
