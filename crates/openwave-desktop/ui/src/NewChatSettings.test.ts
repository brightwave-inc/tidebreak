// @vitest-environment jsdom
import { describe, expect, it } from "vitest";

import type { ApiClient } from "./api";
import {
  effectiveNewChatSettings,
  useNewChatSettings,
} from "./NewChatSettings";

/**
 * Persistence of the pre-chat choices lives on the server (the sticky
 * `chat_default.*` settings), not in this store. What must hold here is the
 * precedence the pickers and `POST /chats` both rely on: an explicit pick
 * this visit beats the server default, an unpicked field follows it, and a
 * failed defaults read degrades to the hard defaults instead of blocking.
 */
describe("new chat settings", () => {
  it("prefers this visit's pick, then the server default, then the hard default", async () => {
    const client = {
      getSettings: () =>
        Promise.resolve({
          model: null,
          has_api_key: true,
          chat_defaults: {
            model: "anthropic::m-sticky",
            reasoning_effort: "high",
            permission_mode: "allow",
            network_policy: { mode: "package_managers" },
          },
        }),
    } as unknown as ApiClient;

    await useNewChatSettings.getState().loadDefaults(client);
    let effective = effectiveNewChatSettings(useNewChatSettings.getState());
    expect(effective.model).toBe("anthropic::m-sticky");
    expect(effective.permissionMode).toBe("allow");
    expect(effective.networkPolicy).toEqual({ mode: "package_managers" });

    useNewChatSettings.getState().setPermissionMode("plan");
    effective = effectiveNewChatSettings(useNewChatSettings.getState());
    expect(effective.permissionMode).toBe("plan");
    // The explicit pick is what the create request will carry; the rest stay
    // unsent and seed server-side.
    expect(useNewChatSettings.getState().permissionMode).toBe("plan");
    expect(useNewChatSettings.getState().networkPolicy).toBeNull();

    // A failed refresh keeps the last known defaults on display.
    const failing = {
      getSettings: () => Promise.reject(new Error("offline")),
    } as unknown as ApiClient;
    await useNewChatSettings.getState().loadDefaults(failing);
    effective = effectiveNewChatSettings(useNewChatSettings.getState());
    expect(effective.networkPolicy).toEqual({ mode: "package_managers" });
  });
});
