// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import {
  describeImageAttachment,
  imageFilesFrom,
  refuseStrayFileDrops,
  transferCarriesFiles,
  imageAttachmentName,
  imageAttachmentRefusal,
  imageAttachmentRejection,
  imageUploadPercent,
  imageUploadsInFlight,
  parseAttachedImage,
  parsePublishedImage,
  queuedImageAttachment,
  readyImageAttachment,
  readyImageAttachmentIds,
  readyTranscriptImageAttachments,
  withRetryQueued,
  withUploadFailed,
  withUploadProgress,
  withUploadPublished,
  withUploadStarted,
  withoutAttachment,
  type ImageAttachment,
} from "./ImageAttachments";

const ATTACHMENT_ID = "1c2f1a44-2f3b-4a1e-9f0a-2b6d5c4e3a21";
const OTHER_ATTACHMENT_ID = "2d3e2b55-3a4c-4b2f-8e1b-3c7e6d5f4b32";

function published(attachmentId = ATTACHMENT_ID) {
  return {
    attachmentId,
    mediaType: "image/png" as const,
    width: 800,
    height: 600,
    byteLen: 2_048,
  };
}

function queued(id: string, byteLen = 1_000): ImageAttachment {
  return queuedImageAttachment(id, {
    name: `${id}.png`,
    byteLen,
    previewUrl: `blob:${id}`,
  });
}

describe("image attachment state machine", () => {
  it("walks one attachment from attached to sendable", () => {
    let list = [queued("a")];
    expect(list[0].status).toBe("queued");
    expect(imageUploadsInFlight(list)).toBe(true);
    expect(readyImageAttachmentIds(list)).toEqual([]);

    list = withUploadStarted(list, "a");
    expect(list[0].status).toBe("uploading");

    list = withUploadProgress(list, "a", 500);
    expect(imageUploadPercent(list[0])).toBe(50);

    list = withUploadPublished(list, "a", published());
    expect(list[0].status).toBe("ready");
    expect(imageUploadsInFlight(list)).toBe(false);
    expect(readyImageAttachmentIds(list)).toEqual([ATTACHMENT_ID]);
    expect(describeImageAttachment(list[0])).toBe("800 × 600");
  });

  it("keeps a failed attachment visible and puts it back in line on retry", () => {
    let list = withUploadStarted([queued("a")], "a");
    list = withUploadProgress(list, "a", 400);
    list = withUploadFailed(list, "a", "That image file is damaged");

    // The chip stays: a message sent without an image the reader attached is
    // the failure this is preventing.
    expect(list).toHaveLength(1);
    expect(list[0].status).toBe("failed");
    expect(describeImageAttachment(list[0])).toBe("That image file is damaged");
    expect(imageUploadsInFlight(list)).toBe(false);
    // Nor is it sendable while it is broken.
    expect(readyImageAttachmentIds(list)).toEqual([]);

    list = withRetryQueued(list, "a");
    expect(list[0]).toMatchObject({
      status: "queued",
      uploadedBytes: 0,
      error: null,
      previewUrl: "blob:a",
    });
    list = withUploadStarted(list, "a");
    list = withUploadPublished(list, "a", published());
    expect(readyImageAttachmentIds(list)).toEqual([ATTACHMENT_ID]);
  });

  it("only retries what actually failed", () => {
    const uploading = withUploadStarted([queued("a")], "a");
    expect(withRetryQueued(uploading, "a")[0].status).toBe("uploading");
    const ready = withUploadPublished(uploading, "a", published());
    expect(withRetryQueued(ready, "a")[0].status).toBe("ready");
  });

  it("ignores everything addressed to an attachment the reader removed", () => {
    const list = withoutAttachment(withUploadStarted([queued("a")], "a"), "a");
    expect(list).toEqual([]);
    // A progress tick or a completion that arrives after removal must not put
    // the chip back on screen.
    expect(withUploadProgress(list, "a", 900)).toEqual([]);
    expect(withUploadPublished(list, "a", published())).toEqual([]);
    expect(withUploadFailed(list, "a", "boom")).toEqual([]);
  });

  it("never lets progress go backwards or past the total", () => {
    let list = withUploadStarted([queued("a", 1_000)], "a");
    list = withUploadProgress(list, "a", 900);
    list = withUploadProgress(list, "a", 100);
    expect(list[0].uploadedBytes).toBe(900);
    list = withUploadProgress(list, "a", 5_000);
    expect(list[0].uploadedBytes).toBe(1_000);
    expect(imageUploadPercent(list[0])).toBe(100);
    // A queued attachment is not receiving bytes yet.
    expect(withUploadProgress([queued("b")], "b", 500)[0].uploadedBytes).toBe(
      0,
    );
  });

  it("touches only the attachment it names", () => {
    const list = withUploadStarted([queued("a"), queued("b")], "b");
    expect(list.map((item) => item.status)).toEqual(["queued", "uploading"]);
  });

  it("names each published image once, however many chips point at it", () => {
    // Attachment identity is derived from the bytes, so the same screenshot
    // attached twice is one image with two chips.
    const list = [
      readyImageAttachment("a", { ...published(), fileName: "one.png" }),
      readyImageAttachment("b", { ...published(), fileName: "two.png" }),
      readyImageAttachment("c", {
        ...published(OTHER_ATTACHMENT_ID),
        fileName: "three.png",
      }),
    ];
    expect(readyImageAttachmentIds(list)).toEqual([
      ATTACHMENT_ID,
      OTHER_ATTACHMENT_ID,
    ]);
    expect(readyTranscriptImageAttachments(list)).toEqual([
      {
        attachmentId: ATTACHMENT_ID,
        mediaType: "image/png",
        width: 800,
        height: 600,
      },
      {
        attachmentId: OTHER_ATTACHMENT_ID,
        mediaType: "image/png",
        width: 800,
        height: 600,
      },
    ]);
  });
});

describe("attaching images", () => {
  it("replaces a name that says nothing about the image", () => {
    const at = new Date("2026-07-27T12:34:56.789Z");
    // A pasted screenshot arrives as a generic name, or none at all.
    expect(
      imageAttachmentName({ name: "image.png", type: "image/png" }, at),
    ).toBe("pasted-image-2026-07-27-12-34-56.png");
    expect(imageAttachmentName({ name: "", type: "image/jpeg" }, at)).toBe(
      "pasted-image-2026-07-27-12-34-56.jpeg",
    );
    expect(imageAttachmentName({ type: "image/webp" }, at)).toBe(
      "pasted-image-2026-07-27-12-34-56.webp",
    );
    // A name the reader would recognize is left alone.
    expect(
      imageAttachmentName(
        { name: "quarterly-chart.png", type: "image/png" },
        at,
      ),
    ).toBe("quarterly-chart.png");
  });

  it("refuses a batch before any bytes move", () => {
    expect(imageAttachmentRejection([], [])).toBeNull();
    expect(
      imageAttachmentRejection([], [{ type: "image/png", size: 10 }]),
    ).toBeNull();
    expect(
      imageAttachmentRejection([], [{ type: "application/pdf", size: 10 }]),
    ).toMatch(/PNG, JPEG, WebP, or GIF/);
    expect(
      imageAttachmentRejection(
        [],
        [{ type: "image/png", size: 17 * 1024 * 1024 }],
      ),
    ).toMatch(/16 MB or smaller/);
    expect(
      imageAttachmentRejection([], [{ type: "image/png", size: 0 }]),
    ).toMatch(/empty/);
    const full = Array.from({ length: 16 }, (_, index) => queued(`a${index}`));
    expect(
      imageAttachmentRejection(full, [{ type: "image/png", size: 10 }]),
    ).toMatch(/at most 16 images/);
  });

  it("gives every server refusal its own sentence", () => {
    const kinds = [
      "image_attachment_empty",
      "image_attachment_too_large",
      "image_attachment_not_an_image",
      "image_attachment_unsupported_format",
      "image_attachment_media_type_mismatch",
      "image_attachment_dimensions_too_large",
    ];
    const generic = imageAttachmentRefusal("something_new");
    for (const kind of kinds) {
      expect(imageAttachmentRefusal(kind)).not.toBe(generic);
    }
  });
});

describe("drags and drops", () => {
  it("reads a drag from its advertised types and a drop from its files", () => {
    // `files` is empty until the drop lands, so the hint has to come from the
    // types the drag advertises.
    const dragging = { types: ["Files"], files: [] } as unknown as DataTransfer;
    expect(transferCarriesFiles(dragging)).toBe(true);
    expect(
      transferCarriesFiles({
        types: ["text/plain"],
      } as unknown as DataTransfer),
    ).toBe(false);
    expect(transferCarriesFiles(null)).toBe(false);

    const dropped = {
      types: ["Files"],
      files: [
        new File([""], "chart.png", { type: "image/png" }),
        new File([""], "notes.txt", { type: "text/plain" }),
      ],
    } as unknown as DataTransfer;
    expect(imageFilesFrom(dropped).map((file) => file.name)).toEqual([
      "chart.png",
    ]);
    expect(imageFilesFrom(null)).toEqual([]);
  });

  it("makes a drop the app did not claim inert instead of navigating", () => {
    const dragOver = (dropEffect: string) => {
      const transfer = { dropEffect } as DataTransfer;
      const event = Object.assign(
        new Event("dragover", { bubbles: true, cancelable: true }),
        { dataTransfer: transfer },
      );
      window.dispatchEvent(event);
      return { event, transfer };
    };

    const stop = refuseStrayFileDrops(window);
    const stray = new Event("drop", { bubbles: true, cancelable: true });
    window.dispatchEvent(stray);
    // Without this the webview navigates away from the app to show the file.
    expect(stray.defaultPrevented).toBe(true);
    expect(dragOver("copy").transfer.dropEffect).toBe("none");

    // A surface that took the drop itself keeps its own answer.
    const claim = (event: Event) => event.preventDefault();
    window.addEventListener("dragover", claim, { capture: true });
    expect(dragOver("copy").transfer.dropEffect).toBe("copy");
    window.removeEventListener("dragover", claim, { capture: true });

    stop();
    const afterStop = new Event("drop", { bubbles: true, cancelable: true });
    window.dispatchEvent(afterStop);
    expect(afterStop.defaultPrevented).toBe(false);
  });
});

describe("image attachment responses", () => {
  it("accepts what the host and the server actually send", () => {
    expect(
      parseAttachedImage({
        attachmentId: ATTACHMENT_ID,
        fileName: "beach.png",
        mediaType: "image/png",
        width: 800,
        height: 600,
        byteLen: 2_048,
      }),
    ).toEqual({ ...published(), fileName: "beach.png" });
    // Dismissing the picker is a normal outcome, not a failure.
    expect(parseAttachedImage(null)).toBeNull();
    expect(
      parsePublishedImage({
        attachment_id: ATTACHMENT_ID,
        media_type: "image/png",
        width: 800,
        height: 600,
        byte_len: 2_048,
      }),
    ).toEqual(published());
  });

  it("rejects a response the composer could not safely render", () => {
    const valid = {
      attachmentId: ATTACHMENT_ID,
      fileName: "beach.png",
      mediaType: "image/png",
      width: 800,
      height: 600,
      byteLen: 2_048,
    };
    // A name carrying a bidi override could redraw the rest of the chip.
    expect(() =>
      parseAttachedImage({ ...valid, fileName: "gnp.\u{202e}png" }),
    ).toThrow();
    expect(() => parseAttachedImage({ ...valid, fileName: "" })).toThrow();
    expect(() =>
      parseAttachedImage({ ...valid, mediaType: "image/svg+xml" }),
    ).toThrow();
    expect(() => parseAttachedImage({ ...valid, width: 0 })).toThrow();
    expect(() => parseAttachedImage({ ...valid, height: 8_001 })).toThrow();
    expect(() =>
      parseAttachedImage({ ...valid, attachmentId: "nope" }),
    ).toThrow();
    // An unexpected field means this is not the shape it claims to be.
    expect(() => parseAttachedImage({ ...valid, path: "/Users/me" })).toThrow();
  });
});
