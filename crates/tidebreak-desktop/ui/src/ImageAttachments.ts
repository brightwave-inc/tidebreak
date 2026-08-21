/**
 * Composer image attachments: what one is, how it moves between states, and the
 * two ways its bytes reach the server.
 *
 * An attachment is published before the turn that carries it, so the composer
 * holds renderer-local identity (`id`) for a while before the server hands back
 * durable identity (`attachmentId`). Everything here is written around that gap:
 * the chip exists, and can be named, removed, and retried, from the moment the
 * user attaches something.
 */

/**
 * How many images one turn may carry, mirroring the server's ceiling so the
 * refusal lands on the attach that broke it rather than on send.
 */
export const MAX_IMAGE_ATTACHMENTS = 16;

/** Per-image byte ceiling, mirroring the server's. */
export const MAX_IMAGE_ATTACHMENT_BYTES = 16 * 1024 * 1024;

/** Per-side pixel ceiling, mirroring the server's. */
export const MAX_IMAGE_DIMENSION = 8_000;

/** The formats the server will publish. */
export const IMAGE_MEDIA_TYPES = [
  "image/png",
  "image/jpeg",
  "image/webp",
  "image/gif",
] as const;

export type ImageMediaType = (typeof IMAGE_MEDIA_TYPES)[number];

/** Durable identity and geometry for one published image. */
export type PublishedImage = {
  attachmentId: string;
  mediaType: ImageMediaType;
  width: number;
  height: number;
  byteLen: number;
};

/** The durable image detail a message needs after the composer has cleared. */
export type TranscriptImageAttachment = {
  attachmentId: string;
  mediaType: string;
  width: number;
  height: number;
};

/** A published image plus the name the host picker read it under. */
export type PickedImage = PublishedImage & { fileName: string };

export type ImageAttachmentStatus = "queued" | "uploading" | "ready" | "failed";

export type ImageAttachment = {
  /**
   * Renderer-local identity, minted when the user attaches and never reused.
   * The chip is addressable — removable, retryable — before the server has an
   * opinion about the bytes, which is the whole point of not keying on
   * `attachmentId`.
   */
  id: string;
  /** What the chip calls this image. Never sent anywhere. */
  name: string;
  /** Total bytes, or `null` while only the host knows them. */
  byteLen: number | null;
  uploadedBytes: number;
  status: ImageAttachmentStatus;
  /**
   * Object URL for a locally held file, or `null` when the bytes never entered
   * the renderer — images chosen through the native picker are read and
   * uploaded entirely in the host process, so there is nothing here to preview
   * and the chip falls back to format and geometry.
   */
  previewUrl: string | null;
  /** Server identity, set once the upload is accepted. */
  attachmentId: string | null;
  mediaType: string | null;
  width: number | null;
  height: number | null;
  error: string | null;
};

/** A newly attached image, before any bytes have moved. */
export function queuedImageAttachment(
  id: string,
  file: { name: string; byteLen: number; previewUrl: string | null },
): ImageAttachment {
  return {
    id,
    name: file.name,
    byteLen: file.byteLen,
    uploadedBytes: 0,
    status: "queued",
    previewUrl: file.previewUrl,
    attachmentId: null,
    mediaType: null,
    width: null,
    height: null,
    error: null,
  };
}

/** An image the host picked, read, and published in one step. */
export function readyImageAttachment(
  id: string,
  published: PickedImage,
): ImageAttachment {
  return {
    id,
    name: published.fileName,
    byteLen: published.byteLen,
    uploadedBytes: published.byteLen,
    status: "ready",
    previewUrl: null,
    attachmentId: published.attachmentId,
    mediaType: published.mediaType,
    width: published.width,
    height: published.height,
    error: null,
  };
}

/**
 * Every transition below is a no-op for an id the list no longer holds. That is
 * what makes removal safe mid-upload: a progress tick or a late completion for
 * a chip the user already dismissed finds nothing to write to, rather than
 * resurrecting it.
 */
function mapAttachment(
  attachments: readonly ImageAttachment[],
  id: string,
  change: (attachment: ImageAttachment) => ImageAttachment,
): ImageAttachment[] {
  return attachments.map((attachment) =>
    attachment.id === id ? change(attachment) : attachment,
  );
}

export function withUploadStarted(
  attachments: readonly ImageAttachment[],
  id: string,
): ImageAttachment[] {
  return mapAttachment(attachments, id, (attachment) =>
    attachment.status === "queued"
      ? { ...attachment, status: "uploading", uploadedBytes: 0, error: null }
      : attachment,
  );
}

/**
 * Progress only ever moves forward and never past the total. A browser that
 * reports the final `loaded` twice, or a retry that starts over, must not make
 * the bar jump backwards under the reader.
 */
export function withUploadProgress(
  attachments: readonly ImageAttachment[],
  id: string,
  uploadedBytes: number,
): ImageAttachment[] {
  return mapAttachment(attachments, id, (attachment) => {
    if (attachment.status !== "uploading") return attachment;
    const capped =
      attachment.byteLen === null
        ? uploadedBytes
        : Math.min(uploadedBytes, attachment.byteLen);
    const next = Math.max(attachment.uploadedBytes, capped);
    return next === attachment.uploadedBytes
      ? attachment
      : { ...attachment, uploadedBytes: next };
  });
}

export function withUploadPublished(
  attachments: readonly ImageAttachment[],
  id: string,
  published: PublishedImage,
): ImageAttachment[] {
  return mapAttachment(attachments, id, (attachment) => ({
    ...attachment,
    status: "ready",
    byteLen: published.byteLen,
    uploadedBytes: published.byteLen,
    attachmentId: published.attachmentId,
    mediaType: published.mediaType,
    width: published.width,
    height: published.height,
    error: null,
  }));
}

/**
 * A failed attachment keeps its place in the strip. A chip that vanishes on
 * error tells the reader nothing went wrong and quietly sends a turn without
 * the image they attached.
 */
export function withUploadFailed(
  attachments: readonly ImageAttachment[],
  id: string,
  message: string,
): ImageAttachment[] {
  return mapAttachment(attachments, id, (attachment) => ({
    ...attachment,
    status: "failed",
    uploadedBytes: 0,
    error: message,
  }));
}

/** Put a failed attachment back in line for another attempt. */
export function withRetryQueued(
  attachments: readonly ImageAttachment[],
  id: string,
): ImageAttachment[] {
  return mapAttachment(attachments, id, (attachment) =>
    attachment.status === "failed"
      ? { ...attachment, status: "queued", uploadedBytes: 0, error: null }
      : attachment,
  );
}

export function withoutAttachment(
  attachments: readonly ImageAttachment[],
  id: string,
): ImageAttachment[] {
  return attachments.filter((attachment) => attachment.id !== id);
}

/** Whether bytes are still moving for any attachment. */
export function imageUploadsInFlight(
  attachments: readonly ImageAttachment[],
): boolean {
  return attachments.some(
    (attachment) =>
      attachment.status === "queued" || attachment.status === "uploading",
  );
}

/**
 * The ids a turn should carry, in display order.
 *
 * Deduplicated because attachment identity is content-derived: attaching the
 * same screenshot twice yields two chips holding one id, and naming it twice
 * would ask the model to look at the same picture two times.
 */
export function readyImageAttachmentIds(
  attachments: readonly ImageAttachment[],
): string[] {
  const ids = attachments
    .filter((attachment) => attachment.status === "ready")
    .map((attachment) => attachment.attachmentId)
    .filter((id): id is string => id !== null);
  return [...new Set(ids)];
}

/**
 * Convert ready composer chips into the stable identity and geometry that the
 * optimistic user message can retain after the composer clears. Duplicate
 * content-addressed ids are folded just as they are in the turn request.
 */
export function readyTranscriptImageAttachments(
  attachments: readonly ImageAttachment[],
): TranscriptImageAttachment[] {
  const seen = new Set<string>();
  return attachments.flatMap((attachment) => {
    if (
      attachment.status !== "ready" ||
      attachment.attachmentId === null ||
      attachment.mediaType === null ||
      attachment.width === null ||
      attachment.height === null ||
      seen.has(attachment.attachmentId)
    ) {
      return [];
    }
    seen.add(attachment.attachmentId);
    return [
      {
        attachmentId: attachment.attachmentId,
        mediaType: attachment.mediaType,
        width: attachment.width,
        height: attachment.height,
      },
    ];
  });
}

/** Whole-percent upload progress for the chip's bar. */
export function imageUploadPercent(attachment: ImageAttachment): number {
  if (attachment.status === "ready") return 100;
  if (!attachment.byteLen) return 0;
  const ratio = attachment.uploadedBytes / attachment.byteLen;
  return Math.max(0, Math.min(100, Math.round(ratio * 100)));
}

/** The one line under the file name that says where this attachment stands. */
export function describeImageAttachment(attachment: ImageAttachment): string {
  switch (attachment.status) {
    case "queued":
      return "Waiting to upload";
    case "uploading":
      // A host publish moves the bytes over IPC in one step and has no progress
      // to report, so quoting 0% would read as a stall rather than as work.
      return attachment.uploadedBytes === 0
        ? "Uploading"
        : `Uploading ${imageUploadPercent(attachment)}%`;
    case "ready":
      return attachment.width && attachment.height
        ? `${attachment.width} × ${attachment.height}`
        : "Ready to send";
    case "failed":
      return attachment.error ?? "Upload failed";
  }
}

/**
 * A name worth showing for a dropped or pasted image.
 *
 * A pasted screenshot arrives unnamed, or as a generic `image.png` the browser
 * invented. Three of those in the strip are indistinguishable, so anything that
 * carries no information is replaced with the moment it was pasted.
 */
export function imageAttachmentName(
  file: { name?: string; type: string },
  at: Date,
): string {
  const given = file.name?.trim() ?? "";
  if (given && !/^image\.(png|jpe?g|webp|gif)$/i.test(given)) return given;
  const stamp = at
    .toISOString()
    .slice(0, 19)
    .replace("T", "-")
    .replace(/:/g, "-");
  const extension = file.type.split("/")[1] || "png";
  return `pasted-image-${stamp}.${extension}`;
}

export function isSupportedImageType(type: string): type is ImageMediaType {
  return (IMAGE_MEDIA_TYPES as readonly string[]).includes(type);
}

/**
 * Why this batch of files cannot be attached, or `null` when it can.
 *
 * Checked before any bytes move so the reader learns the file is too large from
 * the file they just dropped, not from a failed chip a second later.
 */
export function imageAttachmentRejection(
  attached: readonly ImageAttachment[],
  files: readonly { type: string; size: number }[],
): string | null {
  if (files.length === 0) return null;
  if (attached.length + files.length > MAX_IMAGE_ATTACHMENTS) {
    return `A message can carry at most ${MAX_IMAGE_ATTACHMENTS} images.`;
  }
  if (files.some((file) => !isSupportedImageType(file.type))) {
    return "Attach a PNG, JPEG, WebP, or GIF image.";
  }
  if (files.some((file) => file.size > MAX_IMAGE_ATTACHMENT_BYTES)) {
    return "Images must be 16 MB or smaller.";
  }
  if (files.some((file) => file.size === 0)) {
    return "That image file is empty.";
  }
  return null;
}

/**
 * Refuse file drops everywhere except where something is listening for them.
 *
 * The webview, not the host, receives file drops, and its own answer to one is
 * to navigate away from the app and display the file — which unmounts the whole
 * UI and loses the draft. A composer claims its own drops; this makes every
 * other square inch of the window inert.
 */
export function refuseStrayFileDrops(target: EventTarget): () => void {
  const refuse = (event: Event) => {
    // A surface that took the drop itself has already said so.
    if (event.defaultPrevented) return;
    if (event.type === "dragover") {
      const transfer = (event as DragEvent).dataTransfer;
      if (transfer) transfer.dropEffect = "none";
    }
    event.preventDefault();
  };
  target.addEventListener("dragover", refuse);
  target.addEventListener("drop", refuse);
  return () => {
    target.removeEventListener("dragover", refuse);
    target.removeEventListener("drop", refuse);
  };
}

/** The image files carried by a drop or a paste, in the order they arrived. */
export function imageFilesFrom(transfer: DataTransfer | null): File[] {
  if (!transfer) return [];
  return [...transfer.files].filter((file) => file.type.startsWith("image/"));
}

/**
 * Whether a drag in progress is carrying files at all.
 *
 * `files` is empty until the drop actually happens — the browser will not let a
 * page read what is being dragged over it — so the drop hint has to be decided
 * from the advertised types instead.
 */
export function transferCarriesFiles(transfer: DataTransfer | null): boolean {
  return transfer !== null && [...transfer.types].includes("Files");
}

/**
 * Turn a machine-readable server refusal into something worth reading.
 *
 * These sentences mirror the ones the host produces for the native picker so a
 * reader gets the same answer about the same file whichever way they attached
 * it. Each distinct reason gets its own sentence: "that file is not an image"
 * and "that image is too large" call for different actions.
 */
export function imageAttachmentRefusal(kind: string): string {
  switch (kind) {
    case "image_attachment_empty":
      return "That image file is empty";
    case "image_attachment_too_large":
    case "payload_too_large":
      return "Images must be 16 MB or smaller";
    case "image_attachment_not_an_image":
      return "That file is not an image";
    case "image_attachment_unsupported_format":
      return "Attach a PNG, JPEG, WebP, or GIF image";
    case "image_attachment_media_type_mismatch":
    case "image_attachment_media_type_required":
      return "That file's contents do not match its type";
    case "image_attachment_zero_dimension":
    case "image_attachment_unreadable":
      return "That image file is damaged";
    case "image_attachment_dimensions_too_large":
      return "Images must be 8000 pixels or smaller on a side";
    default:
      return "Could not attach that image";
  }
}

/**
 * Publish one locally held image and report how much of it has gone out.
 *
 * `XMLHttpRequest` rather than `fetch` because only it reports how far a
 * request body has got. The byte count is known here, so an indeterminate
 * shimmer would be hiding information the composer already has.
 */
export function uploadImageAttachment(
  server: { baseUrl: string; token: string },
  targetId: string,
  file: File,
  options: {
    onProgress: (uploadedBytes: number) => void;
    signal: AbortSignal;
    path?: (id: string) => string;
  },
): Promise<PublishedImage> {
  return new Promise((resolve, reject) => {
    if (options.signal.aborted) {
      reject(new DOMException("Upload cancelled", "AbortError"));
      return;
    }
    const request = new XMLHttpRequest();
    const path =
      options.path?.(targetId) ?? `/chats/${targetId}/attachments/images`;
    request.open("POST", `${server.baseUrl}${path}`);
    request.setRequestHeader("Authorization", `Bearer ${server.token}`);
    request.setRequestHeader("Content-Type", file.type);
    request.upload.onprogress = (event) => {
      if (event.lengthComputable) options.onProgress(event.loaded);
    };
    request.onload = () => {
      if (request.status !== 201) {
        reject(
          new Error(refusalFromBody(request.status, request.responseText)),
        );
        return;
      }
      try {
        resolve(parsePublishedImage(JSON.parse(request.responseText)));
      } catch {
        reject(new Error("Attaching the image returned an invalid response"));
      }
    };
    request.onerror = () => reject(new Error("Could not attach that image"));
    request.onabort = () =>
      reject(new DOMException("Upload cancelled", "AbortError"));
    options.signal.addEventListener("abort", () => request.abort(), {
      once: true,
    });
    request.send(file);
  });
}

function refusalFromBody(status: number, body: string): string {
  try {
    const parsed: unknown = JSON.parse(body);
    if (isRecord(parsed) && typeof parsed.kind === "string") {
      return imageAttachmentRefusal(parsed.kind);
    }
  } catch {
    /* An unparseable body is just an unknown refusal. */
  }
  // The auth middleware answers with a bare status and no body, so this is the
  // one refusal that carries no `kind` to explain itself. Saying so is what
  // separates "this build cannot publish from the renderer" from a dead
  // network — the two used to arrive as the same sentence.
  if (status === 401 || status === 403) {
    return "This build cannot attach images from the composer";
  }
  return imageAttachmentRefusal("");
}

/**
 * The host's picker result. `null` means the user dismissed the dialog, which
 * is a normal outcome rather than a failure.
 */
export function parseAttachedImage(value: unknown): PickedImage | null {
  if (value === null) return null;
  if (
    !isExactRecord(value, [
      "attachmentId",
      "fileName",
      "mediaType",
      "width",
      "height",
      "byteLen",
    ])
  ) {
    throw new Error("Invalid image attachment response");
  }
  if (
    typeof value.fileName !== "string" ||
    !isSafeRendererText(value.fileName, 255)
  ) {
    throw new Error("Invalid image attachment response");
  }
  return {
    fileName: value.fileName,
    ...checkedPublishedImage({
      attachmentId: value.attachmentId,
      mediaType: value.mediaType,
      width: value.width,
      height: value.height,
      byteLen: value.byteLen,
    }),
  };
}

/**
 * Whether a host-supplied string is safe to put straight into the composer.
 *
 * A file name is chosen by whoever wrote the file, not by us: control and
 * formatting characters in one can reorder or hide the rest of the chip's text.
 */
function isSafeRendererText(value: string, maxCodePoints: number): boolean {
  const characters = [...value];
  if (characters.length === 0 || characters.length > maxCodePoints)
    return false;
  return characters.every(
    (character) => !/[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]/u.test(character),
  );
}

/**
 * The host's publish result for an image the renderer already held.
 *
 * Same five numbers as the endpoint's own answer, in the camelCase every host
 * command uses. There is no file name here: the renderer named the paste before
 * any bytes moved, and the host has no better name to offer for one.
 */
export function parseHostPublishedImage(value: unknown): PublishedImage {
  if (
    !isExactRecord(value, [
      "attachmentId",
      "mediaType",
      "width",
      "height",
      "byteLen",
    ])
  ) {
    throw new Error("Invalid image attachment response");
  }
  return checkedPublishedImage({
    attachmentId: value.attachmentId,
    mediaType: value.mediaType,
    width: value.width,
    height: value.height,
    byteLen: value.byteLen,
  });
}

/** The publish endpoint's result, which uses the server's snake_case shape. */
export function parsePublishedImage(value: unknown): PublishedImage {
  if (
    !isExactRecord(value, [
      "attachment_id",
      "media_type",
      "width",
      "height",
      "byte_len",
    ])
  ) {
    throw new Error("Invalid image attachment response");
  }
  return checkedPublishedImage({
    attachmentId: value.attachment_id,
    mediaType: value.media_type,
    width: value.width,
    height: value.height,
    byteLen: value.byte_len,
  });
}

function checkedPublishedImage(candidate: {
  attachmentId: unknown;
  mediaType: unknown;
  width: unknown;
  height: unknown;
  byteLen: unknown;
}): PublishedImage {
  const { attachmentId, mediaType, width, height, byteLen } = candidate;
  if (
    !isUuid(attachmentId) ||
    typeof mediaType !== "string" ||
    !isSupportedImageType(mediaType) ||
    !isBoundedInteger(width, 1, MAX_IMAGE_DIMENSION) ||
    !isBoundedInteger(height, 1, MAX_IMAGE_DIMENSION) ||
    !isBoundedInteger(byteLen, 1, MAX_IMAGE_ATTACHMENT_BYTES)
  ) {
    throw new Error("Invalid image attachment response");
  }
  return { attachmentId, mediaType, width, height, byteLen };
}

function isBoundedInteger(
  value: unknown,
  minimum: number,
  maximum: number,
): value is number {
  return (
    typeof value === "number" &&
    Number.isInteger(value) &&
    value >= minimum &&
    value <= maximum
  );
}

function isUuid(value: unknown): value is string {
  return (
    typeof value === "string" &&
    /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(
      value,
    )
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isExactRecord(
  value: unknown,
  keys: readonly string[],
): value is Record<string, unknown> {
  if (!isRecord(value)) return false;
  const actual = Object.keys(value);
  return (
    actual.length === keys.length && actual.every((key) => keys.includes(key))
  );
}
