// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";
import { toast } from "sonner";

import {
  clearQueuedComposerAttachments,
  queueComposerMessage,
} from "./ChatRoute";
import { useComposerDrafts } from "./ComposerDrafts";

vi.mock("sonner", () => ({ toast: { error: vi.fn() } }));

beforeEach(() => {
  useComposerDrafts.setState({ drafts: {}, attachments: {} });
});

describe("queueing a message behind a running turn", () => {
  it("keeps the text and speaks up when the queue refuses it", async () => {
    // A queued message gets no optimistic bubble, so a refusal used to be
    // completely silent: the composer emptied and the message was gone.
    const onQueued = vi.fn();
    await queueComposerMessage(async () => {
      throw new Error("chat is not accepting messages");
    }, onQueued);

    expect(onQueued).not.toHaveBeenCalled();
    expect(toast.error).toHaveBeenCalledWith("chat is not accepting messages");
  });

  it("clears files and skills once the queue accepts the turn", () => {
    useComposerDrafts.setState({
      drafts: {},
      attachments: {
        "chat-1": {
          images: [],
          files: [
            {
              documentId: "doc-1",
              displayName: "notes.pdf",
              mediaType: "application/pdf",
              byteLen: 10,
            },
          ],
          pastedTexts: [{ id: "paste-1", text: "held" }],
          skills: ["review"],
          folders: ["root-1"],
          pendingChatId: null,
        },
      },
    });

    clearQueuedComposerAttachments("chat-1", new Set(["paste-1"]));

    expect(useComposerDrafts.getState().attachments["chat-1"]).toBeUndefined();
  });
});
