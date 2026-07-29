// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

/**
 * The pre-chat choices are read back from storage a session later, so what is
 * stored there is untrusted input: a value this build no longer knows must
 * read as "unset" rather than reach chat creation, where it would be refused
 * by the server or — worse for a permission mode — mean something other than
 * what the reader chose.
 */
describe("new chat settings", () => {
  beforeEach(() => {
    vi.resetModules();
    window.localStorage.clear();
  });

  it("recovers stored choices and drops ones it no longer recognizes", async () => {
    window.localStorage.setItem("openwave.new-chat-permission-mode", "auto");
    window.localStorage.setItem("openwave.new-chat-reasoning-effort", "sideways");

    const { useNewChatSettings } = await import("./NewChatSettings");
    const state = useNewChatSettings.getState();

    expect(state.permissionMode).toBe("auto");
    expect(state.reasoningEffort).toBeNull();
  });

  it("persists a choice so it outlives the visit that made it", async () => {
    const { useNewChatSettings } = await import("./NewChatSettings");

    useNewChatSettings.getState().setPermissionMode("allow");

    expect(useNewChatSettings.getState().permissionMode).toBe("allow");
    expect(window.localStorage.getItem("openwave.new-chat-permission-mode")).toBe(
      "allow",
    );
  });
});
