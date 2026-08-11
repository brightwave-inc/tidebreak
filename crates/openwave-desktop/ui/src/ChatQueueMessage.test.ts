import { describe, expect, it, vi } from "vitest";
import { toast } from "sonner";

import { queueComposerMessage } from "./ChatRoute";

vi.mock("sonner", () => ({ toast: { error: vi.fn() } }));

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

  it("clears the composer once the message is on the queue", async () => {
    const onQueued = vi.fn();
    await queueComposerMessage(async () => undefined, onQueued);
    expect(onQueued).toHaveBeenCalled();
  });
});
