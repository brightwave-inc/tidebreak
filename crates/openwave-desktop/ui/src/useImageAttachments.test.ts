// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "./api";
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

function png(name = "chart.png"): File {
  return new File([new Uint8Array([1, 2, 3, 4])], name, { type: "image/png" });
}

beforeEach(() => {
  FakeUpload.opened = [];
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
});
