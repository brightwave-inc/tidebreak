import { describe, expect, it, vi } from "vitest";

import { applyPendingChatSettings, singleFlight } from "./HomeRoute";

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
  memory_incognito: false,
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
    expect(patchChatReasoningEffort).toHaveBeenCalledWith(
      "pending-chat",
      "high",
    );
    expect(patchChatPermissionMode).toHaveBeenCalledWith(
      "pending-chat",
      "plan",
    );
    expect(patchChatNetworkPolicy).toHaveBeenCalledWith("pending-chat", {
      mode: "off",
    });
    expect(patchChatPermissionMode).toHaveBeenCalledBefore(
      patchChatNetworkPolicy,
    );
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
          model: "anthropic::claude-opus-4",
          reasoningEffort: null,
          permissionMode: "plan",
          networkPolicy: { mode: "off" },
        },
      ),
    ).rejects.toThrow("model update rejected");
    expect(patchChatPermissionMode).not.toHaveBeenCalled();
    expect(patchChatNetworkPolicy).not.toHaveBeenCalled();
  });

  /**
   * Loading the home defaults is allowed to fail quietly, which leaves the
   * picker showing "default" with nothing resolved behind it. Sending null
   * would clear the model the server seeded from the sticky default and run
   * the turn on the global one instead, with nothing on screen to say so.
   */
  it("leaves a server-seeded model alone when home resolved none", async () => {
    const patchChatModel = vi.fn(async () => pendingChat);
    const patchChatReasoningEffort = vi.fn(async () => pendingChat);
    const patchChatPermissionMode = vi.fn(async () => pendingChat);
    const patchChatNetworkPolicy = vi.fn(async () => pendingChat);

    await applyPendingChatSettings(
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
    );

    expect(patchChatModel).not.toHaveBeenCalled();
    expect(patchChatReasoningEffort).not.toHaveBeenCalled();
    expect(patchChatPermissionMode).toHaveBeenCalledWith(
      "pending-chat",
      "plan",
    );
  });
});

/**
 * Home has no conversation until an attachment forces one, and every route in
 * asks for it independently: each file of a dropped batch, each pasted image,
 * the paperclip. Creating one per caller leaves empty chats in the sidebar
 * that the reader never asked for and has to clean up.
 */
describe("creating home's pending chat", () => {
  it("creates one chat for a batch that asks for it all at once", async () => {
    const create = vi.fn(async () => "chat-1");
    const ensure = singleFlight<string>();

    const batch = await Promise.all([
      ensure(create),
      ensure(create),
      ensure(create),
    ]);

    expect(create).toHaveBeenCalledOnce();
    expect(batch).toEqual(["chat-1", "chat-1", "chat-1"]);
  });

  it("lets the next attachment try again after a failed creation", async () => {
    const create = vi
      .fn<() => Promise<string>>()
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce("chat-1");
    const ensure = singleFlight<string>();

    await expect(ensure(create)).rejects.toThrow("offline");
    await expect(ensure(create)).resolves.toBe("chat-1");
    expect(create).toHaveBeenCalledTimes(2);
  });
});
