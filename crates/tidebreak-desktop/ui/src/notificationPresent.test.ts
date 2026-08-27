import { describe, expect, it } from "vitest";

import {
  notificationPresent,
  viewingNotificationConversation,
} from "./notificationPresent";

describe("notificationPresent", () => {
  const elsewhere = {
    windowFocused: true,
    viewingConversation: false,
    permission: "granted" as const,
  };

  it("skips when the main window is focused on that conversation", () => {
    expect(
      notificationPresent({
        windowFocused: true,
        viewingConversation: true,
        permission: "granted",
      }),
    ).toBe("skip");
  });

  it("toasts when the window is focused elsewhere", () => {
    expect(notificationPresent(elsewhere)).toBe("toast");
    expect(notificationPresent({ ...elsewhere, permission: "denied" })).toBe(
      "toast",
    );
  });

  it("uses a native banner when the window is unfocused and permitted", () => {
    expect(
      notificationPresent({
        windowFocused: false,
        viewingConversation: false,
        permission: "granted",
      }),
    ).toBe("native");
    expect(
      notificationPresent({
        windowFocused: false,
        viewingConversation: true,
        permission: "prompt",
      }),
    ).toBe("native");
  });

  it("falls back to a dock bounce when permission is denied", () => {
    expect(
      notificationPresent({
        windowFocused: false,
        viewingConversation: false,
        permission: "denied",
      }),
    ).toBe("dock");
    expect(
      notificationPresent({
        windowFocused: false,
        viewingConversation: false,
        permission: "unavailable",
      }),
    ).toBe("dock");
  });

  it("never chooses toast and native for the same row", () => {
    const kinds = [
      notificationPresent({
        windowFocused: true,
        viewingConversation: false,
        permission: "granted",
      }),
      notificationPresent({
        windowFocused: false,
        viewingConversation: false,
        permission: "granted",
      }),
      notificationPresent({
        windowFocused: true,
        viewingConversation: true,
        permission: "granted",
      }),
      notificationPresent({
        windowFocused: false,
        viewingConversation: false,
        permission: "denied",
      }),
    ];
    expect(kinds.filter((kind) => kind === "toast")).toHaveLength(1);
    expect(kinds.filter((kind) => kind === "native")).toHaveLength(1);
    expect(new Set(kinds).size).toBe(kinds.length);
  });

  it("skips when desktop notifications are turned off", () => {
    expect(notificationPresent({ ...elsewhere, enabled: false })).toBe("skip");
  });
});

describe("viewingNotificationConversation", () => {
  it("matches a work chat and a project chat URL", () => {
    expect(
      viewingNotificationConversation("/c/chat-1", {
        surface: "chat",
        chatId: "chat-1",
      }),
    ).toBe(true);
    expect(
      viewingNotificationConversation("/p/proj/c/chat-1", {
        surface: "chat",
        chatId: "chat-1",
      }),
    ).toBe(true);
    expect(
      viewingNotificationConversation("/c/chat-2", {
        surface: "chat",
        chatId: "chat-1",
      }),
    ).toBe(false);
  });

  it("matches a code workspace URL", () => {
    expect(
      viewingNotificationConversation("/code/w/ws-1", {
        surface: "code",
        sessionId: "s-1",
        workspaceId: "ws-1",
      }),
    ).toBe(true);
    expect(
      viewingNotificationConversation("/code/delivery/pull-requests", {
        surface: "code",
        sessionId: "s-1",
        workspaceId: "ws-1",
      }),
    ).toBe(false);
  });
});
