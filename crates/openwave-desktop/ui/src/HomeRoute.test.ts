import { describe, expect, it, vi } from "vitest";

import { applyPendingChatSettings } from "./HomeRoute";

const pendingChat = {
  id: "pending-chat",
  title: null,
  model: null,
  reasoning_effort: null,
  permission_mode: "allow" as const,
  network_policy: { mode: "open" as const },
  project_id: null,
  created_at: "2026-08-10T00:00:00Z",
  attachment_revision: 0,
  root_attachments: [],
};

describe("home attachment settings", () => {
  it("applies attachment → restrict → send settings to the pending server chat", async () => {
    const patchChatModel = vi.fn(async () => pendingChat);
    const patchChatReasoningEffort = vi.fn(async () => pendingChat);
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
      {
        patchChatModel,
        patchChatReasoningEffort,
        patchChatPermissionMode,
        patchChatNetworkPolicy,
      },
      pendingChat.id,
      // The user restricts both controls before sending their first message,
      // and picks a different model to run it on.
      {
        model: "anthropic::claude-opus-4",
        reasoningEffort: "high",
        permissionMode: "plan",
        networkPolicy: { mode: "off" },
      },
    );

    // The model the composer was showing at send is the one the turn runs on:
    // the chat was created back when the picker still said something else.
    expect(patchChatModel).toHaveBeenCalledWith(
      "pending-chat",
      "anthropic::claude-opus-4",
    );
    expect(patchChatReasoningEffort).toHaveBeenCalledWith("pending-chat", "high");
    expect(patchChatPermissionMode).toHaveBeenCalledWith("pending-chat", "plan");
    expect(patchChatNetworkPolicy).toHaveBeenCalledWith("pending-chat", {
      mode: "off",
    });
    expect(patchChatPermissionMode).toHaveBeenCalledBefore(patchChatNetworkPolicy);
  });

  it("surfaces a rejected pending-chat update to the sender", async () => {
    const patchChatModel = vi.fn(async () => {
      throw new Error("model update rejected");
    });
    const patchChatReasoningEffort = vi.fn();
    const patchChatPermissionMode = vi.fn();
    const patchChatNetworkPolicy = vi.fn();

    await expect(
      applyPendingChatSettings(
        {
          patchChatModel,
          patchChatReasoningEffort,
          patchChatPermissionMode,
          patchChatNetworkPolicy,
        },
        pendingChat.id,
        {
          model: null,
          reasoningEffort: null,
          permissionMode: "plan",
          networkPolicy: { mode: "off" },
        },
      ),
    ).rejects.toThrow("model update rejected");
    expect(patchChatPermissionMode).not.toHaveBeenCalled();
    expect(patchChatNetworkPolicy).not.toHaveBeenCalled();
  });
});
