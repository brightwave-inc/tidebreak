import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../api/client";

const publishCodeImage = vi.hoisted(() => vi.fn());
const uploadImageAttachment = vi.hoisted(() => vi.fn());
const hasLocalHostAuthority = vi.hoisted(() => vi.fn(() => false));

vi.mock("../attachments", () => ({
  publishCodeImage,
}));

vi.mock("../ImageAttachments", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../ImageAttachments")>()),
  uploadImageAttachment,
}));

vi.mock("../host", () => ({
  hasLocalHostAuthority,
}));

import {
  publishCodeSessionImages,
  submitFirstCodeTurn,
} from "./publishCodeSessionImages";

const SESSION_A = "sess-new-1";
const SESSION_B = "sess-other";
const BLOB_ID = "1c2f1a44-2f3b-4a1e-9f0a-2b6d5c4e3a21";

function pngFile(): File {
  return new File([new Uint8Array([1, 2, 3, 4])], "shot.png", {
    type: "image/png",
  });
}

describe("publishCodeSessionImages / submitFirstCodeTurn", () => {
  beforeEach(() => {
    publishCodeImage.mockReset();
    uploadImageAttachment.mockReset();
    hasLocalHostAuthority.mockReset();
    hasLocalHostAuthority.mockReturnValue(false);
    uploadImageAttachment.mockResolvedValue({
      attachmentId: BLOB_ID,
      mediaType: "image/png",
      width: 1,
      height: 1,
      byteLen: 4,
    });
  });

  it("publishes each file to the new session before any turn is submitted", async () => {
    const order: string[] = [];
    uploadImageAttachment.mockImplementation(
      async (_client: ApiClient, sessionId: string) => {
        order.push(`publish:${sessionId}`);
        return {
          attachmentId: BLOB_ID,
          mediaType: "image/png",
          width: 1,
          height: 1,
          byteLen: 4,
        };
      },
    );
    const submitCodeTurn = vi.fn(async () => {
      order.push("submit");
      return { kind: "turn" };
    });
    const client = { submitCodeTurn } as unknown as ApiClient;
    const file = pngFile();

    await submitFirstCodeTurn({
      client,
      sessionId: SESSION_A,
      message: "look at this",
      images: [file],
    });

    expect(order).toEqual([`publish:${SESSION_A}`, "submit"]);
    expect(uploadImageAttachment).toHaveBeenCalledWith(
      client,
      SESSION_A,
      file,
      expect.objectContaining({
        path: expect.any(Function),
      }),
    );
    const path = uploadImageAttachment.mock.calls[0][3].path as (
      id: string,
    ) => string;
    expect(path(SESSION_A)).toBe(
      `/sessions/${encodeURIComponent(SESSION_A)}/attachments/images`,
    );
    expect(submitCodeTurn).toHaveBeenCalledTimes(1);
    expect(submitCodeTurn).toHaveBeenCalledWith(
      SESSION_A,
      "look at this",
      undefined,
      [{ blob_id: BLOB_ID, media_type: "image/png" }],
    );
  });

  it("refuses to bind an image published for another session into the turn", async () => {
    // Without the session-id argument on publish, a caller could reserve
    // bytes on session B and then name that blob on session A's first turn.
    // The server would answer 400 "was not published to session". This test
    // locks the client contract: publication target === turn session.
    uploadImageAttachment.mockImplementation(
      async (_client: ApiClient, sessionId: string) => {
        expect(sessionId).toBe(SESSION_A);
        expect(sessionId).not.toBe(SESSION_B);
        return {
          attachmentId: BLOB_ID,
          mediaType: "image/png",
          width: 1,
          height: 1,
          byteLen: 4,
        };
      },
    );
    const submitCodeTurn = vi.fn(async () => ({ kind: "turn" }));
    const client = { submitCodeTurn } as unknown as ApiClient;

    await submitFirstCodeTurn({
      client,
      sessionId: SESSION_A,
      message: "first turn",
      images: [pngFile()],
    });

    expect(submitCodeTurn).toHaveBeenCalledWith(
      SESSION_A,
      "first turn",
      undefined,
      [{ blob_id: BLOB_ID, media_type: "image/png" }],
    );
  });

  it("submits text-only first turns once, with no attachment payload", async () => {
    const submitCodeTurn = vi.fn(async () => ({ kind: "turn" }));
    const client = { submitCodeTurn } as unknown as ApiClient;

    await submitFirstCodeTurn({
      client,
      sessionId: SESSION_A,
      message: "  hello  ",
      images: [],
    });

    expect(uploadImageAttachment).not.toHaveBeenCalled();
    expect(submitCodeTurn).toHaveBeenCalledTimes(1);
    expect(submitCodeTurn).toHaveBeenCalledWith(SESSION_A, "hello");
  });

  it("does not submit when the message is empty", async () => {
    const submitCodeTurn = vi.fn();
    const client = { submitCodeTurn } as unknown as ApiClient;

    const posted = await submitFirstCodeTurn({
      client,
      sessionId: SESSION_A,
      message: "   ",
      images: [pngFile()],
    });

    expect(posted).toBe(false);
    expect(submitCodeTurn).not.toHaveBeenCalled();
    expect(uploadImageAttachment).not.toHaveBeenCalled();
  });

  it("uses the host publish path when local authority is available", async () => {
    hasLocalHostAuthority.mockReturnValue(true);
    publishCodeImage.mockResolvedValue({
      attachmentId: BLOB_ID,
      mediaType: "image/png",
      width: 1,
      height: 1,
      byteLen: 4,
    });
    const file = pngFile();
    const client = {} as ApiClient;

    const attachments = await publishCodeSessionImages(client, SESSION_A, [
      file,
    ]);

    expect(publishCodeImage).toHaveBeenCalledWith(SESSION_A, file);
    expect(uploadImageAttachment).not.toHaveBeenCalled();
    expect(attachments).toEqual([
      { blob_id: BLOB_ID, media_type: "image/png" },
    ]);
  });
});
