// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "./api";
import { useComposerDrafts } from "./ComposerDrafts";
import { useImageAttachments } from "./useImageAttachments";

const ATTACHMENT_ID = "1c2f1a44-2f3b-4a1e-9f0a-2b6d5c4e3a21";

const PUBLISHED = JSON.stringify({
  attachment_id: ATTACHMENT_ID,
  media_type: "image/png",
  width: 800,
  height: 600,
  byte_len: 4,
});

/** Enough of `XMLHttpRequest` to drive an upload one step at a time. */
class FakeUpload {
  static opened: FakeUpload[] = [];

  method = "";
  url = "";
  status = 0;
  responseText = "";
  body: unknown = null;
  headers: Record<string, string> = {};
  aborted = false;
  upload = { onprogress: null as ((event: ProgressEvent) => void) | null };
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onabort: (() => void) | null = null;

  open(method: string, url: string) {
    this.method = method;
    this.url = url;
  }

  setRequestHeader(name: string, value: string) {
    this.headers[name] = value;
  }

  send(body: unknown) {
    this.body = body;
    FakeUpload.opened.push(this);
  }

  abort() {
    this.aborted = true;
    this.onabort?.();
  }

  progress(loaded: number) {
    this.upload.onprogress?.({ lengthComputable: true, loaded } as ProgressEvent);
  }

  finish(status: number, responseText: string) {
    this.status = status;
    this.responseText = responseText;
    this.onload?.();
  }
}

const client = { baseUrl: "http://127.0.0.1:9", token: "t" } as ApiClient;

const hasNativeHost = vi.hoisted(() => vi.fn(() => false));
const publishChatImage = vi.hoisted(() => vi.fn());

vi.mock("./host", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./host")>()),
  hasNativeHost,
}));
vi.mock("./attachments", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./attachments")>()),
  publishChatImage,
}));

function png(name = "chart.png"): File {
  return new File([new Uint8Array([1, 2, 3, 4])], name, { type: "image/png" });
}

beforeEach(() => {
  // The attachments live in the module-level draft store now, so each test
  // starts it empty rather than inheriting the last test's chips.
  useComposerDrafts.setState({ drafts: {}, attachments: {} });
  window.sessionStorage.clear();
  FakeUpload.opened = [];
  hasNativeHost.mockReturnValue(false);
  publishChatImage.mockReset();
  vi.stubGlobal("XMLHttpRequest", FakeUpload);
  URL.createObjectURL = vi.fn((): string => "blob:preview");
  URL.revokeObjectURL = vi.fn();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("useImageAttachments", () => {
  it("uploads a dropped file, reporting the bytes that have actually gone out", async () => {
    const { result } = renderHook(() => useImageAttachments(client, "chat-1"));

    act(() => result.current.attachFiles([png()]));
    await waitFor(() => expect(FakeUpload.opened).toHaveLength(1));
    const upload = FakeUpload.opened[0];
    expect(upload.method).toBe("POST");
    expect(upload.url).toBe(
      "http://127.0.0.1:9/chats/chat-1/attachments/images",
    );
    expect(upload.headers["Content-Type"]).toBe("image/png");
    expect(upload.headers.Authorization).toBe("Bearer t");
    expect(result.current.attachments[0]).toMatchObject({
      name: "chart.png",
      status: "uploading",
      previewUrl: "blob:preview",
    });

    act(() => upload.progress(2));
    expect(result.current.attachments[0].uploadedBytes).toBe(2);

    act(() => upload.finish(201, PUBLISHED));
    await waitFor(() =>
      expect(result.current.attachments[0]).toMatchObject({
        status: "ready",
        attachmentId: ATTACHMENT_ID,
        width: 800,
      }),
    );
  });

  it("keeps a refused upload retryable from the file it already holds", async () => {
    const { result } = renderHook(() => useImageAttachments(client, "chat-1"));

    act(() => result.current.attachFiles([png()]));
    await waitFor(() => expect(FakeUpload.opened).toHaveLength(1));
    act(() =>
      FakeUpload.opened[0].finish(
        400,
        JSON.stringify({ kind: "image_attachment_unreadable" }),
      ),
    );

    await waitFor(() =>
      expect(result.current.attachments[0]).toMatchObject({
        status: "failed",
        error: "That image file is damaged",
      }),
    );

    // Retry has to reuse the file the composer kept. Asking the reader to find
    // it again would be a fresh attach, not a retry.
    act(() => result.current.retry(result.current.attachments[0].id));
    await waitFor(() => expect(FakeUpload.opened).toHaveLength(2));
    expect((FakeUpload.opened[1].body as File).name).toBe("chart.png");

    act(() => FakeUpload.opened[1].finish(201, PUBLISHED));
    await waitFor(() =>
      expect(result.current.attachments[0]).toMatchObject({
        status: "ready",
        attachmentId: ATTACHMENT_ID,
      }),
    );
  });

  it("gives back the preview and stops the upload when an image is removed", async () => {
    const { result } = renderHook(() => useImageAttachments(client, "chat-1"));

    act(() => result.current.attachFiles([png()]));
    await waitFor(() => expect(FakeUpload.opened).toHaveLength(1));

    act(() => result.current.remove(result.current.attachments[0].id));
    expect(result.current.attachments).toEqual([]);
    expect(FakeUpload.opened[0].aborted).toBe(true);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:preview");
    // The cancelled upload must not reappear as a failed chip.
    await waitFor(() => expect(result.current.attachments).toEqual([]));
  });

  it("refuses a batch that cannot be attached without touching the network", () => {
    const { result } = renderHook(() => useImageAttachments(client, "chat-1"));

    act(() =>
      result.current.attachFiles([
        new File(["x"], "notes.pdf", { type: "application/pdf" }),
      ]),
    );
    expect(result.current.error).toMatch(/PNG, JPEG, WebP, or GIF/);
    expect(result.current.attachments).toEqual([]);
    expect(FakeUpload.opened).toHaveLength(0);
  });

  // Regression: the packaged app has no bearer for the publish endpoint, so a
  // pasted image posted from the renderer came back 401 with an empty body and
  // reached the reader as the generic "Could not attach that image".
  it("publishes through the host rather than the renderer when there is one", async () => {
    hasNativeHost.mockReturnValue(true);
    publishChatImage.mockResolvedValue({
      attachmentId: ATTACHMENT_ID,
      mediaType: "image/png",
      width: 800,
      height: 600,
      byteLen: 4,
    });
    const { result } = renderHook(() => useImageAttachments(client, "chat-1"));

    act(() => result.current.attachFiles([png()]));
    await waitFor(() =>
      expect(result.current.attachments[0]).toMatchObject({
        status: "ready",
        attachmentId: ATTACHMENT_ID,
        width: 800,
      }),
    );
    expect(FakeUpload.opened).toHaveLength(0);
    expect(publishChatImage).toHaveBeenCalledWith("chat-1", expect.any(File));
  });

  it("keeps the strip across the remount a chat switch causes", async () => {
    const first = renderHook(() => useImageAttachments(client, "chat-1"));

    act(() => first.result.current.attachFiles([png()]));
    await waitFor(() => expect(FakeUpload.opened).toHaveLength(1));
    act(() => FakeUpload.opened[0].finish(201, PUBLISHED));
    await waitFor(() =>
      expect(first.result.current.attachments[0].status).toBe("ready"),
    );

    first.unmount();
    const second = renderHook(() => useImageAttachments(client, "chat-1"));
    expect(second.result.current.attachments[0]).toMatchObject({
      name: "chart.png",
      status: "ready",
      attachmentId: ATTACHMENT_ID,
    });

    // A chat the reader switched away from mid-upload lands ready when they
    // return, rather than dying with the unmounted route.
    act(() => second.result.current.attachFiles([png("later.png")]));
    await waitFor(() => expect(FakeUpload.opened).toHaveLength(2));
    second.unmount();
    act(() => FakeUpload.opened[1].finish(201, PUBLISHED));

    const third = renderHook(() => useImageAttachments(client, "chat-1"));
    await waitFor(() =>
      expect(third.result.current.attachments[1]).toMatchObject({
        name: "later.png",
        status: "ready",
      }),
    );
  });

  it("forgets everything once a turn has carried it", async () => {
    const { result } = renderHook(() => useImageAttachments(client, "chat-1"));

    act(() => result.current.attachFiles([png()]));
    await waitFor(() => expect(FakeUpload.opened).toHaveLength(1));
    act(() => FakeUpload.opened[0].finish(201, PUBLISHED));
    await waitFor(() =>
      expect(result.current.attachments[0].status).toBe("ready"),
    );

    act(() => result.current.clear());
    expect(result.current.attachments).toEqual([]);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:preview");
  });

  it("puts ready chips back after a refused send without their local preview", async () => {
    const { result } = renderHook(() => useImageAttachments(client, "chat-1"));

    act(() => result.current.attachFiles([png()]));
    await waitFor(() => expect(FakeUpload.opened).toHaveLength(1));
    act(() => FakeUpload.opened[0].finish(201, PUBLISHED));
    await waitFor(() =>
      expect(result.current.attachments[0].status).toBe("ready"),
    );
    const held = result.current.attachments;

    act(() => result.current.clear());
    act(() => result.current.restore(held));
    expect(result.current.attachments).toHaveLength(1);
    expect(result.current.attachments[0]).toMatchObject({
      status: "ready",
      previewUrl: null,
      attachmentId: ATTACHMENT_ID,
    });
  });
});
