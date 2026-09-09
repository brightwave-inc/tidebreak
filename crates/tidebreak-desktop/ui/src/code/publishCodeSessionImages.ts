import type { ApiClient } from "../api/client";
import { publishCodeImage } from "../attachments";
import { hasLocalHostAuthority } from "../host";
import { uploadImageAttachment } from "../ImageAttachments";

/** One image reference a code turn can carry after publication. */
export type CodeTurnImageAttachment = {
  blob_id: string;
  media_type: string;
};

/**
 * Publish image files to a code session and return turn attachment refs.
 *
 * Publication is per-session authority: bytes must be reserved on the session
 * that will submit the turn. Callers hold `File`s until that session exists,
 * then publish here before `POST …/turns`. Publishing to a different session
 * (or skipping publication) yields `attachment blob … was not published`.
 */
export async function publishCodeSessionImages(
  client: ApiClient,
  sessionId: string,
  files: readonly File[],
): Promise<readonly CodeTurnImageAttachment[]> {
  if (files.length === 0) return [];
  const published = await Promise.all(
    files.map(async (file) => {
      if (hasLocalHostAuthority()) {
        return publishCodeImage(sessionId, file);
      }
      return uploadImageAttachment(client, sessionId, file, {
        onProgress: () => undefined,
        signal: new AbortController().signal,
        path: (id) => `/sessions/${encodeURIComponent(id)}/attachments/images`,
      });
    }),
  );
  return published.map((image) => ({
    blob_id: image.attachmentId,
    media_type: image.mediaType,
  }));
}

/**
 * Submit one first turn, publishing any images to `sessionId` first.
 *
 * Returns whether a turn was posted. Callers that already created the session
 * use this so the message is sent at most once and only with attachments the
 * new session actually holds.
 */
export async function submitFirstCodeTurn(input: {
  client: ApiClient;
  sessionId: string;
  message: string;
  images?: readonly File[];
}): Promise<boolean> {
  const message = input.message.trim();
  if (!message) return false;
  const attachments = await publishCodeSessionImages(
    input.client,
    input.sessionId,
    input.images ?? [],
  );
  if (attachments.length > 0) {
    await input.client.submitCodeTurn(
      input.sessionId,
      message,
      undefined,
      attachments,
    );
  } else {
    await input.client.submitCodeTurn(input.sessionId, message);
  }
  return true;
}
