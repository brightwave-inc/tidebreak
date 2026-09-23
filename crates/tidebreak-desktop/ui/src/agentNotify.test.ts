// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { toast } from "sonner";

import { presentFinished, presentNeedsYou } from "./agentNotify";
import type { AgentNotification } from "./api";
import { forgetBannerTarget, takeBannerTarget } from "./bannerTarget";
import * as host from "./host";
import type { NeedsYouQuestion } from "./needsYou";
import {
  setFinishedNotificationsEnabled,
  setNeedsYouNotificationsEnabled,
} from "./NotificationPreferences";

vi.mock("./host", () => ({
  isWindowFocused: vi.fn(),
  presentNativeNotification: vi.fn(),
  requestUserAttention: vi.fn(),
}));

vi.mock("sonner", () => ({ toast: vi.fn() }));

function parked(
  overrides: Partial<Parameters<typeof presentNeedsYou>[0]> = {},
  question: NeedsYouQuestion = {
    kind: "approval",
    text: "Run this command? npm test",
  },
) {
  return {
    name: "Fix the login check",
    href: "/c/chat-2",
    viewing: false,
    question: vi.fn(async () => question),
    stillWaiting: () => true,
    ...overrides,
  };
}

const finished: AgentNotification = {
  id: "notification-1",
  kind: "agent_completed",
  title: "Research finished",
  body: "Three files changed.",
  context: { surface: "chat", chatId: "chat-1" },
  createdAt: "2026-09-23T10:00:00Z",
  readAt: null,
};

beforeEach(() => {
  vi.mocked(host.isWindowFocused).mockReset();
  vi.mocked(host.presentNativeNotification).mockReset().mockResolvedValue(true);
  vi.mocked(host.requestUserAttention).mockReset().mockResolvedValue();
  vi.mocked(toast).mockClear();
  window.localStorage.clear();
  forgetBannerTarget();
});

afterEach(() => {
  forgetBannerTarget();
});

describe("an agent that needs you", () => {
  it("posts a banner, bounces the Dock until you return, and remembers the conversation", async () => {
    vi.mocked(host.isWindowFocused).mockResolvedValue(false);

    await expect(presentNeedsYou(parked())).resolves.toBe("native");

    expect(host.presentNativeNotification).toHaveBeenCalledWith(
      "Needs your approval: Fix the login check",
      "Run this command? npm test",
    );
    expect(host.requestUserAttention).toHaveBeenCalledWith({ critical: true });
    expect(toast).not.toHaveBeenCalled();
    expect(takeBannerTarget()).toBe("/c/chat-2");
  });

  it("stays quiet while you are looking at that conversation", async () => {
    vi.mocked(host.isWindowFocused).mockResolvedValue(true);
    const notice = parked({ viewing: true });

    await expect(presentNeedsYou(notice)).resolves.toBe("skip");

    // Skipping costs nothing: the question is not even read.
    expect(notice.question).not.toHaveBeenCalled();
    expect(host.presentNativeNotification).not.toHaveBeenCalled();
    expect(host.requestUserAttention).not.toHaveBeenCalled();
    expect(toast).not.toHaveBeenCalled();
    expect(takeBannerTarget()).toBeNull();
  });

  it("still posts a banner for the conversation on screen once you switch apps", async () => {
    vi.mocked(host.isWindowFocused).mockResolvedValue(false);

    await expect(presentNeedsYou(parked({ viewing: true }))).resolves.toBe(
      "native",
    );
    expect(host.requestUserAttention).toHaveBeenCalledWith({ critical: true });
  });

  it("toasts the question when you are elsewhere in the app", async () => {
    vi.mocked(host.isWindowFocused).mockResolvedValue(true);

    await presentNeedsYou(
      parked({}, { kind: "question", text: "Which database should I use?" }),
    );

    expect(toast).toHaveBeenCalledWith(
      "Needs your answer: Fix the login check",
      expect.objectContaining({
        description: "Which database should I use?",
        action: expect.objectContaining({ label: "Open" }),
      }),
    );
    expect(host.presentNativeNotification).not.toHaveBeenCalled();
    expect(host.requestUserAttention).not.toHaveBeenCalled();
  });

  it("does nothing when you turned the notification off", async () => {
    vi.mocked(host.isWindowFocused).mockResolvedValue(false);
    setNeedsYouNotificationsEnabled(false);

    await expect(presentNeedsYou(parked())).resolves.toBe("skip");
    expect(host.presentNativeNotification).not.toHaveBeenCalled();
    expect(host.requestUserAttention).not.toHaveBeenCalled();
  });
});

describe("an agent that finished", () => {
  it("puts the row's body under its title and remembers where it opens", async () => {
    vi.mocked(host.isWindowFocused).mockResolvedValue(false);

    await presentFinished(finished, {
      viewing: false,
      stillUnread: () => true,
      onOpen: vi.fn(),
    });

    expect(host.presentNativeNotification).toHaveBeenCalledWith(
      "Research finished",
      "Three files changed.",
    );
    // A finished agent is news, not a blocker: no Dock bounce on top.
    expect(host.requestUserAttention).not.toHaveBeenCalled();
    expect(takeBannerTarget()).toBe("/c/chat-1");
  });

  it("follows its own switch, not the one for agents that need you", async () => {
    vi.mocked(host.isWindowFocused).mockResolvedValue(false);
    setNeedsYouNotificationsEnabled(false);
    setFinishedNotificationsEnabled(true);

    await expect(
      presentFinished(finished, {
        viewing: false,
        stillUnread: () => true,
        onOpen: vi.fn(),
      }),
    ).resolves.toBe("native");

    setFinishedNotificationsEnabled(false);
    await expect(
      presentFinished(finished, {
        viewing: false,
        stillUnread: () => true,
        onOpen: vi.fn(),
      }),
    ).resolves.toBe("skip");
  });
});
