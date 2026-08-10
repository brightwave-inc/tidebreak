import { describe, expect, it, vi } from "vitest";

import { applyPendingChatSettings } from "./HomeRoute";

const pendingChat = {
  id: "pending-chat",
  title: null,
  model: null,
  reasoning_effort: null,
  permission_mode: "allow",
  network_policy: { mode: "open" as const },
  project_id: null,
  created_at: "2026-08-10T00:00:00Z",
  attachment_revision: 0,
  root_attachments: [],
};

describe("home attachment settings", () => {
  it("applies attachment → restrict → send settings to the pending server chat", async () => {
    const patchChatPermissionMode = vi.fn(async () => ({
      ...pendingChat,
      permission_mode: "plan" as const,
    }));
    const patchChatNetworkPolicy = vi.fn(async () => ({
      ...pendingChat,
      permission_mode: "plan" as const,
      network_policy: { mode: "off" as const },
    }));

    // Attaching created this chat with the prior, permissive home settings.
    await applyPendingChatSettings(
      { patchChatPermissionMode, patchChatNetworkPolicy },
      pendingChat.id,
      // The user restricts both controls before sending their first message.
      { permissionMode: "plan", networkPolicy: { mode: "off" } },
    );

    expect(patchChatPermissionMode).toHaveBeenCalledWith("pending-chat", "plan");
    expect(patchChatNetworkPolicy).toHaveBeenCalledWith("pending-chat", {
      mode: "off",
    });
    expect(patchChatPermissionMode).toHaveBeenCalledBefore(patchChatNetworkPolicy);
  });

  it("surfaces a rejected pending-chat update to the sender", async () => {
    const patchChatPermissionMode = vi.fn(async () => {
      throw new Error("permission update rejected");
    });
    const patchChatNetworkPolicy = vi.fn();

    await expect(
      applyPendingChatSettings(
        { patchChatPermissionMode, patchChatNetworkPolicy },
        pendingChat.id,
        { permissionMode: "plan", networkPolicy: { mode: "off" } },
      ),
    ).rejects.toThrow("permission update rejected");
    expect(patchChatNetworkPolicy).not.toHaveBeenCalled();
  });
});
