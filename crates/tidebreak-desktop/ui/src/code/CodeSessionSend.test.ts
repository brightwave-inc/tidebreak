import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../api/client";
import { useComposerDrafts } from "../ComposerDrafts";
import { holdComposerImages } from "../useImageAttachments";
import { createCodeSessionStore } from "./CodeSessionStore";
import {
  sendCodeComposer,
  submitAcceptedTurn,
  useCodeComposerStatus,
} from "./CodeSessionSend";
import { userItemId } from "./CodeSessionReducer";

const uploadImageAttachment = vi.hoisted(() => vi.fn());

vi.mock("../ImageAttachments", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../ImageAttachments")>()),
  uploadImageAttachment,
}));

vi.mock("../host", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../host")>()),
  hasLocalHostAuthority: () => false,
}));

const TURN = {
  id: "turn-1",
  session_id: "sess-1",
  ordinal: 1,
  status: "running" as const,
  fast_mode: false,
  user_input: "list the files",
  attachments: [],
  started_at: "2026-08-15T12:00:00.000Z",
};

describe("submitAcceptedTurn", () => {
  it("inserts a turn-keyed user item only after the server accepts", async () => {
    const store = createCodeSessionStore();
    await submitAcceptedTurn(store.getState().update, async () => ({
      kind: "ran" as const,
      turn: TURN,
    }));
    expect(store.getState().items).toEqual([
      {
        kind: "user",
        id: userItemId("turn-1"),
        turnId: "turn-1",
        text: "list the files",
        createdAt: "2026-08-15T12:00:00.000Z",
        attachments: [],
      },
    ]);
  });

  it("leaves the transcript empty when submit fails, so a retry cannot stack", async () => {
    const store = createCodeSessionStore();
    await expect(
      submitAcceptedTurn(store.getState().update, async () => {
        throw new Error("session is fenced");
      }),
    ).rejects.toThrow("session is fenced");
    expect(store.getState().items).toEqual([]);
  });
});

describe("retry after a failed submit", () => {
  it("does not keep a bubble from the failed attempt", async () => {
    const store = createCodeSessionStore();
    const submit = vi
      .fn()
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce({ kind: "ran" as const, turn: TURN });
    await expect(
      submitAcceptedTurn(store.getState().update, submit),
    ).rejects.toThrow("offline");
    expect(store.getState().items).toEqual([]);
    await submitAcceptedTurn(store.getState().update, submit);
    expect(store.getState().items).toHaveLength(1);
    expect(store.getState().items[0]).toMatchObject({
      kind: "user",
      turnId: "turn-1",
      text: "list the files",
    });
  });
});

describe("accepted turn after the socket already painted", () => {
  it("inserts the prompt above the streamed assistant reply", async () => {
    const store = createCodeSessionStore();
    const deps = {
      nextId: () => "streamed",
      now: () => "2026-08-15T12:00:02.000Z",
    };
    store
      .getState()
      .applyEvents(
        [{ seq: 1, event: { type: "turn_started", turn_id: "turn-1" } }],
        deps,
      );
    store
      .getState()
      .applyEvents(
        [{ seq: 2, event: { type: "assistant_delta", text: "README.md" } }],
        deps,
      );
    store.getState().applyEvents(
      [
        {
          seq: 3,
          event: {
            type: "turn_completed",
            usage: {
              input_tokens: 1,
              output_tokens: 1,
              cache_read_input_tokens: 0,
              cache_creation_input_tokens: 0,
              context_tokens: 0,
            },
          },
        },
      ],
      deps,
    );
    await submitAcceptedTurn(store.getState().update, async () => ({
      kind: "ran" as const,
      turn: { ...TURN, status: "completed" as const },
    }));
    expect(store.getState().items.map((item) => item.kind)).toEqual([
      "user",
      "assistant",
      "turn_boundary",
    ]);
    expect(store.getState().items[0]).toMatchObject({
      kind: "user",
      turnId: "turn-1",
      text: "list the files",
    });
  });
});

describe("sendCodeComposer", () => {
  const BLOB = "1c2f1a44-2f3b-4a1e-9f0a-2b6d5c4e3a21";
  const client = {} as ApiClient;
  const pasted = { id: "paste-1", text: "a long paste" };

  function image(): File {
    return new File([new Uint8Array([1, 2, 3, 4])], "shot.png", {
      type: "image/png",
    });
  }

  /** A start surface's composer: typed words, a paste, a held image. */
  function startDraft(key: string) {
    useComposerDrafts.getState().setDraft(key, "look at this");
    useComposerDrafts.getState().setPastedTexts(key, [pasted]);
    expect(holdComposerImages(key, [image()])).toBeNull();
  }

  beforeEach(() => {
    uploadImageAttachment.mockReset();
    uploadImageAttachment.mockImplementation(async () => ({
      attachmentId: BLOB,
      mediaType: "image/png",
      width: 1,
      height: 1,
      byteLen: 4,
    }));
    URL.createObjectURL = vi.fn((): string => "blob:preview");
    URL.revokeObjectURL = vi.fn();
  });

  afterEach(() => {
    useComposerDrafts.setState({ drafts: {}, attachments: {} });
    useCodeComposerStatus.setState({ byKey: {} });
  });

  it("creates the session first and publishes the held image to it before the turn", async () => {
    startDraft("ws-1");
    const order: string[] = [];
    uploadImageAttachment.mockImplementation(
      async (_client: ApiClient, target: string) => {
        order.push(`publish:${target}`);
        return {
          attachmentId: BLOB,
          mediaType: "image/png",
          width: 1,
          height: 1,
          byteLen: 4,
        };
      },
    );
    const send = vi.fn(async (sessionId: string) => {
      order.push(`send:${sessionId}`);
    });

    const sent = await sendCodeComposer({
      client,
      key: "ws-1",
      session: async () => {
        order.push("create");
        return "sess-new";
      },
      send,
    });

    expect(sent).toBe(true);
    // A blob reserved on any other session would be refused as unpublished.
    expect(order).toEqual(["create", "publish:sess-new", "send:sess-new"]);
    expect(send).toHaveBeenCalledWith(
      "sess-new",
      "look at this\n\n<pasted_text>\na long paste\n</pasted_text>",
      [{ blob_id: BLOB, media_type: "image/png" }],
    );
    const state = useComposerDrafts.getState();
    expect(state.drafts["ws-1"]).toBeUndefined();
    expect(state.drafts["sess-new"]).toBeFalsy();
    expect(state.attachments["ws-1"]).toBeUndefined();
    expect(state.attachments["sess-new"]).toBeUndefined();
  });

  it("leaves the draft where it was typed when the session cannot start", async () => {
    startDraft("ws-1");
    const send = vi.fn();

    const sent = await sendCodeComposer({
      client,
      key: "ws-1",
      session: async () => {
        throw new Error("Claude Code sign-in expired");
      },
      send,
    });

    expect(sent).toBe(false);
    expect(send).not.toHaveBeenCalled();
    expect(uploadImageAttachment).not.toHaveBeenCalled();
    const state = useComposerDrafts.getState();
    expect(state.drafts["ws-1"]).toBe("look at this");
    expect(state.attachments["ws-1"]?.pastedTexts).toEqual([pasted]);
    expect(state.attachments["ws-1"]?.images).toEqual([
      expect.objectContaining({ status: "held" }),
    ]);
    expect(useCodeComposerStatus.getState().byKey["ws-1"]).toEqual({
      sending: false,
      notice: "Claude Code sign-in expired",
    });
  });

  it("puts a refused first message in the new session's composer", async () => {
    startDraft("ws-1");

    const sent = await sendCodeComposer({
      client,
      key: "ws-1",
      session: async () => "sess-new",
      send: async () => {
        throw new Error("engine crashed on spawn");
      },
    });

    expect(sent).toBe(false);
    const state = useComposerDrafts.getState();
    expect(state.drafts["ws-1"]).toBeUndefined();
    expect(state.drafts["sess-new"]).toBe("look at this");
    expect(state.attachments["sess-new"]?.pastedTexts).toEqual([pasted]);
    // Already published to this session, so a retry sends it as it is.
    expect(state.attachments["sess-new"]?.images).toEqual([
      expect.objectContaining({ status: "ready", attachmentId: BLOB }),
    ]);
    expect(useCodeComposerStatus.getState().byKey["sess-new"]).toEqual({
      sending: false,
      notice: "engine crashed on spawn",
    });
  });

  it("holds the message while an image that failed to attach is still there", async () => {
    startDraft("sess-1");
    uploadImageAttachment.mockRejectedValue(new Error("too large"));
    const send = vi.fn();

    expect(
      await sendCodeComposer({
        client,
        key: "sess-1",
        session: "sess-1",
        send,
      }),
    ).toBe(false);
    expect(send).not.toHaveBeenCalled();
    expect(useComposerDrafts.getState().drafts["sess-1"]).toBe("look at this");

    expect(
      await sendCodeComposer({
        client,
        key: "sess-1",
        session: "sess-1",
        send,
      }),
    ).toBe(false);
    expect(useCodeComposerStatus.getState().byKey["sess-1"]?.notice).toBe(
      "Remove or retry the images that failed to attach.",
    );
    expect(send).not.toHaveBeenCalled();
  });
});
