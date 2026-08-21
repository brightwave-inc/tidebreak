// @vitest-environment jsdom
import { beforeEach, describe, expect, it } from "vitest";

import { createComposerDraftStore, useComposerDrafts } from "./ComposerDrafts";
import type { ImageAttachment } from "./ImageAttachments";

const READY_IMAGE: ImageAttachment = {
  id: "local-1",
  name: "chart.png",
  byteLen: 4,
  uploadedBytes: 4,
  status: "ready",
  previewUrl: "blob:preview",
  attachmentId: "1c2f1a44-2f3b-4a1e-9f0a-2b6d5c4e3a21",
  mediaType: "image/png",
  width: 800,
  height: 600,
  error: null,
};

const QUEUED_IMAGE: ImageAttachment = {
  ...READY_IMAGE,
  id: "local-2",
  status: "queued",
  attachmentId: null,
};

const IMPORTED_FILE = {
  documentId: "doc-1",
  displayName: "notes.pdf",
  mediaType: "application/pdf",
  byteLen: 10,
};

beforeEach(() => {
  useComposerDrafts.setState({ drafts: {}, attachments: {} });
  window.sessionStorage.clear();
});

describe("composer draft attachments", () => {
  it("restores only what a reload can honestly re-send", () => {
    const store = useComposerDrafts.getState();
    store.setImages("chat-1", [READY_IMAGE, QUEUED_IMAGE]);
    store.setFiles("chat-1", [IMPORTED_FILE]);
    store.setFolders("chat-1", ["root-1"]);

    // A fresh store is what the next load of the page gets.
    const restored =
      createComposerDraftStore().getState().attachments["chat-1"];
    expect(restored.files).toEqual([IMPORTED_FILE]);
    expect(restored.folders).toEqual(["root-1"]);
    // The published image re-sends as-is; the queued one was a promise to
    // move bytes no storage can hold, so no chip comes back for it.
    expect(restored.images).toHaveLength(1);
    expect(restored.images[0]).toMatchObject({
      name: "chart.png",
      status: "ready",
      attachmentId: READY_IMAGE.attachmentId,
      // Object URLs die with the page; the restored chip falls back to
      // format and geometry.
      previewUrl: null,
    });
  });

  it("restores draft folder chips without implying a standing grant", () => {
    const store = useComposerDrafts.getState();
    store.setFolders("chat-1", ["root-1", "root-2"]);

    const restored =
      createComposerDraftStore().getState().attachments["chat-1"];
    expect(restored.folders).toEqual(["root-1", "root-2"]);
    // A connected folder that was never put on this draft does not come back
    // as a chip — send already consumed the strip.
    expect(
      createComposerDraftStore().getState().attachments["chat-2"],
    ).toBeUndefined();
  });

  it("forgets text and attachments together, in the session too", () => {
    const store = useComposerDrafts.getState();
    store.setDraft("chat-1", "half a thought");
    store.setImages("chat-1", [READY_IMAGE]);
    store.clearDraft("chat-1");

    expect(useComposerDrafts.getState().drafts["chat-1"]).toBeUndefined();
    expect(useComposerDrafts.getState().attachments["chat-1"]).toBeUndefined();

    const restored = createComposerDraftStore().getState();
    expect(restored.drafts["chat-1"]).toBeUndefined();
    expect(restored.attachments["chat-1"]).toBeUndefined();
  });
});
