import { create } from "zustand";

import { HttpError, type ApiClient } from "../api/client";
import type { ReasoningEffort } from "../api/types";
import type { ComposerWorkspaceFile } from "../Composer";
import { useComposerDrafts } from "../ComposerDrafts";
import type { ImageAttachment } from "../ImageAttachments";
import {
  messageWithPastedText,
  type PastedTextAttachment,
} from "../PastedText";
import { useRefreshSignals } from "../RefreshSignals";
import {
  forgetComposerImages,
  holdComposerImages,
  moveComposerDraft,
  publishHeldImages,
} from "../useImageAttachments";
import { applyAcceptedTurn, type CodeSessionState } from "./CodeSessionReducer";
import { peekCodeSession } from "./CodeSessionRegistry";
import { messageWithWorkspaceFiles } from "./fork";
import type { CodeTurnSubmission } from "./parsers";

/** One published image a turn carries. */
export type CodeTurnImage = { blob_id: string; media_type: string };

/**
 * Insert a user turn only after the server accepts it.
 *
 * An optimistic bubble that survives a failed submit cannot be answered and
 * stacks a duplicate on retry. Chat removes its optimistic item in the catch;
 * here the cheaper mirror is to wait for the accepted snapshot, which is also
 * the hydrate key.
 *
 * A queued follow-up has no turn row yet, so there is nothing to key an item
 * on. Its bubble arrives when the worker promotes the queued row and the
 * session's `turn_started` event pulls the snapshot in.
 */
export async function submitAcceptedTurn(
  update: (change: (session: CodeSessionState) => CodeSessionState) => void,
  submit: () => Promise<CodeTurnSubmission>,
): Promise<CodeTurnSubmission> {
  const outcome = await submit();
  if (outcome.kind === "ran") {
    update((session) => applyAcceptedTurn(session, outcome.turn));
  }
  return outcome;
}

/**
 * Send one message to a session: the one POST every code composer uses.
 *
 * The server answers once the message is accepted, with the started turn or a
 * queue row. A started turn goes into the session's transcript, if a view has
 * the session open, and its progress arrives on the event socket.
 */
export async function sendCodeTurn(input: {
  client: Pick<ApiClient, "submitCodeTurn">;
  sessionId: string;
  message: string;
  attachments?: readonly CodeTurnImage[];
  model?: string;
  /** Omit to keep the session's level; `null` hands it back to the engine. */
  reasoningEffort?: ReasoningEffort | null;
}): Promise<CodeTurnSubmission> {
  const { client, sessionId, message, attachments, model } = input;
  const outcome = await submitAcceptedTurn(
    (change) => peekCodeSession(sessionId)?.store.getState().update(change),
    () =>
      input.reasoningEffort !== undefined
        ? client.submitCodeTurn(
            sessionId,
            message,
            model,
            attachments,
            input.reasoningEffort,
          )
        : client.submitCodeTurn(sessionId, message, model, attachments),
  );
  // A turn boundary is when the promoter runs, and a parked message is a new
  // row, so the queue tray reads now instead of on its next signal.
  if (outcome.kind === "queued") {
    useRefreshSignals.getState().signal("queuedTurns");
  }
  return outcome;
}

/** Whether a composer is sending, and what it last had to say about a send. */
export type CodeComposerStatus = {
  sending: boolean;
  notice: string | null;
};

const IDLE: CodeComposerStatus = { sending: false, notice: null };

type CodeComposerStatusStore = {
  byKey: Record<string, CodeComposerStatus>;
  set: (key: string, status: CodeComposerStatus) => void;
  move: (from: string, to: string) => void;
};

/**
 * Send state per composer key, beside the draft it belongs to.
 *
 * It lives outside the component for the same reason the draft does: a send
 * can outlast the composer that started it. The start surface unmounts as
 * soon as its session exists, and the session's composer shows the rest of
 * that send, including a refusal.
 */
export const useCodeComposerStatus = create<CodeComposerStatusStore>()(
  (set) => ({
    byKey: {},
    set: (key, status) =>
      set((state) => {
        const byKey = { ...state.byKey };
        if (!status.sending && status.notice === null) delete byKey[key];
        else byKey[key] = status;
        return { byKey };
      }),
    move: (from, to) =>
      set((state) => {
        const moving = state.byKey[from];
        if (!moving || from === to) return state;
        const byKey = { ...state.byKey };
        delete byKey[from];
        byKey[to] = moving;
        return { byKey };
      }),
  }),
);

export function useCodeComposerSendStatus(key: string): CodeComposerStatus {
  return useCodeComposerStatus((state) => state.byKey[key] ?? IDLE);
}

export function setCodeComposerNotice(key: string, notice: string | null) {
  const current = useCodeComposerStatus.getState().byKey[key] ?? IDLE;
  useCodeComposerStatus.getState().set(key, { ...current, notice });
}

function setSending(key: string, sending: boolean, notice: string | null) {
  useCodeComposerStatus.getState().set(key, { sending, notice });
}

/** Why a send did not go, in the words the composer shows. */
export function codeSendFailure(error: unknown): string {
  if (error instanceof HttpError && error.kind === "queue_full") {
    return "The queue is full. Delete a queued message or wait for one to run.";
  }
  return error instanceof Error ? error.message : "Could not send that turn";
}

/**
 * Put a message and images on a composer as if they were typed and pasted
 * there. The images are held until the send publishes them.
 *
 * The new-workspace dialog and Uneff me hand their first message to the new
 * session's composer this way, so it goes through the same send as a message
 * typed into that composer, and a refused send leaves it there to retry.
 */
export function seedCodeComposer(
  key: string,
  message: string,
  images: readonly File[] = [],
): void {
  const text = message.trim();
  const drafts = useComposerDrafts.getState();
  if (text) {
    const current = drafts.drafts[key] ?? "";
    drafts.setDraft(key, current ? `${current.trimEnd()}\n\n${text}` : text);
  }
  if (images.length > 0) {
    const refusal = holdComposerImages(key, images);
    if (refusal) setCodeComposerNotice(key, refusal);
  }
}

/**
 * Send what the composer under `key` holds: its draft, its pasted text, and
 * its images. This is the one send path. The session composer, the start
 * surface, the new-workspace dialog, and Uneff me all come through here.
 *
 * `session` is the session to send to, or a way to create it. The start
 * surface creates its session first. Its draft then moves to that session's
 * composer, which carries the rest of the send: the images publish to the new
 * session with their upload status on the chips, and a refused send leaves
 * everything in that composer to retry.
 *
 * The text and chips stay on screen until the images are published, then
 * leave with the message. A refused send puts them back and says why.
 *
 * Returns whether the server accepted the message.
 */
export async function sendCodeComposer(input: {
  client: ApiClient;
  key: string;
  session: string | (() => Promise<string>);
  /** Files already in the worktree, named after the message. */
  workspaceFiles?: readonly ComposerWorkspaceFile[];
  send: (
    sessionId: string,
    message: string,
    attachments: readonly CodeTurnImage[] | undefined,
  ) => Promise<unknown>;
}): Promise<boolean> {
  let key = input.key;
  if (useCodeComposerStatus.getState().byKey[key]?.sending) return false;
  const drafts = useComposerDrafts.getState();
  const typed = messageWithPastedText(
    drafts.drafts[key] ?? "",
    drafts.attachments[key]?.pastedTexts ?? [],
  );
  if (!typed) return false;
  const message = messageWithWorkspaceFiles(typed, input.workspaceFiles ?? []);
  const images = drafts.attachments[key]?.images ?? [];
  if (
    images.some(
      (item) => item.status === "queued" || item.status === "uploading",
    )
  ) {
    setCodeComposerNotice(key, "Wait for images to finish attaching.");
    return false;
  }
  if (images.some((item) => item.status === "failed")) {
    setCodeComposerNotice(
      key,
      "Remove or retry the images that failed to attach.",
    );
    return false;
  }

  setSending(key, true, null);
  let sessionId: string;
  if (typeof input.session === "string") {
    sessionId = input.session;
  } else {
    try {
      sessionId = await input.session();
    } catch (error) {
      // No session, so nothing moved: the draft is where it was typed.
      setSending(key, false, codeSendFailure(error));
      return false;
    }
    if (!sessionId) {
      setSending(key, false, "The session could not start.");
      return false;
    }
    if (sessionId !== key) {
      moveComposerDraft(key, sessionId);
      useCodeComposerStatus.getState().move(key, sessionId);
      key = sessionId;
    }
  }

  const held = (
    useComposerDrafts.getState().attachments[key]?.images ?? []
  ).some((item) => item.status === "held");
  if (held) {
    try {
      await publishHeldImages(input.client, key, sessionId, "code");
    } catch (error) {
      // The failed chip stays, with its reason and a retry.
      setSending(key, false, codeSendFailure(error));
      return false;
    }
  }

  // The message leaves the composer before the request goes out. A reload or
  // a remount while it is in flight must not offer the same words again.
  const state = useComposerDrafts.getState();
  const sentDraft = state.drafts[key] ?? "";
  const sentPasted = state.attachments[key]?.pastedTexts ?? [];
  const sentImages: readonly ImageAttachment[] =
    state.attachments[key]?.images ?? [];
  const attachments = turnImages(sentImages);
  state.setDraft(key, "");
  state.setPastedTexts(key, []);
  state.setImages(key, []);
  try {
    await input.send(
      sessionId,
      message,
      attachments.length > 0 ? attachments : undefined,
    );
  } catch (error) {
    restoreComposer(key, sentDraft, sentPasted, sentImages);
    setSending(key, false, codeSendFailure(error));
    return false;
  }
  forgetComposerImages(
    key,
    sentImages.map((item) => item.id),
  );
  setSending(key, false, null);
  return true;
}

/** The published images a turn names, deduplicated by blob, in chip order. */
function turnImages(items: readonly ImageAttachment[]): CodeTurnImage[] {
  const seen = new Set<string>();
  const images: CodeTurnImage[] = [];
  for (const item of items) {
    if (item.status !== "ready" || item.attachmentId === null) continue;
    if (seen.has(item.attachmentId)) continue;
    seen.add(item.attachmentId);
    images.push({
      blob_id: item.attachmentId,
      media_type: item.mediaType ?? "image/png",
    });
  }
  return images;
}

/**
 * Put a refused message back, keeping anything typed or attached since. The
 * composer was disabled for the send, so this is almost always the whole
 * draft going back where it was.
 */
function restoreComposer(
  key: string,
  draft: string,
  pastedTexts: readonly PastedTextAttachment[],
  images: readonly ImageAttachment[],
) {
  const state = useComposerDrafts.getState();
  if (!(state.drafts[key] ?? "")) state.setDraft(key, draft);
  const current = state.attachments[key];
  const pastedIds = new Set(
    (current?.pastedTexts ?? []).map((item) => item.id),
  );
  state.setPastedTexts(key, [
    ...pastedTexts.filter((item) => !pastedIds.has(item.id)),
    ...(current?.pastedTexts ?? []),
  ]);
  const imageIds = new Set((current?.images ?? []).map((item) => item.id));
  state.setImages(key, [
    ...images.filter((item) => !imageIds.has(item.id)),
    ...(current?.images ?? []),
  ]);
}
