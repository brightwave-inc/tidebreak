import { toast } from "sonner";

import type { ApiClient } from "../api/client";
import type {
  CodeSessionSnapshot,
  CodeWorkspaceSnapshot,
  HarnessKind,
  ModelInfo,
  PermissionMode,
  ReasoningEffort,
} from "../api/types";
import { publishCodeImage } from "../attachments";
import { uploadImageAttachment } from "../ImageAttachments";
import { hasLocalHostAuthority } from "../host";
import { friendlyErrorMessage } from "@/lib/utils";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeUiStore, type WorkspaceStartup } from "./CodeUiStore";
import {
  gatewayCodeModels,
  preferredCodeModels,
  requiresHarnessModelIds,
} from "./labels";

/** What the first session of a workspace is created with. */
export type FirstSessionSettings = {
  harness: HarnessKind;
  permissionMode: PermissionMode;
  /** A model the reader picked. Omitted, the engine's default is posted. */
  model?: string;
  reasoningEffort?: ReasoningEffort | null;
  fastMode?: boolean;
};

/**
 * The model to post for an engine: the reader's pick when there is one,
 * else the default of the catalog the engine accepts.
 *
 * Engines that only take their own identifiers, and any engine the gateway
 * lists nothing for, are asked for their listing first.
 */
export async function resolveSessionModel(input: {
  client: ApiClient;
  harness: HarnessKind;
  requested?: string;
  models: readonly ModelInfo[];
  defaultModelKey?: string | null;
}): Promise<string | undefined> {
  const gateway = gatewayCodeModels(
    input.models,
    input.harness,
    input.defaultModelKey,
  );
  const native =
    requiresHarnessModelIds(input.harness) || gateway.length === 0
      ? await useCodeCatalogStore
          .getState()
          .ensureHarnessModels(input.client, input.harness)
      : [];
  const listed = preferredCodeModels(input.harness, native, gateway);
  return (
    input.requested ??
    listed.find((option) => option.default)?.id ??
    listed[0]?.id
  );
}

/**
 * Start a workspace's first agent and, when there is one, post its first
 * message, drawing the page-level handoff while that happens.
 *
 * The new-workspace dialog and Uneff me both end here. The handoff is keyed
 * by the workspace, so `reveal` runs first: the page the reader lands on is
 * the one carrying the steps. A failed session leaves the text with the
 * workspace composer, and a failed first turn does the same, so typed words
 * and pasted images are never dropped. The handoff clears on every exit.
 */
export async function startFirstSession(input: {
  client: ApiClient;
  workspace: CodeWorkspaceSnapshot;
  settings: FirstSessionSettings;
  prompt: string;
  images?: readonly File[];
  models: readonly ModelInfo[];
  defaultModelKey?: string | null;
  /** Heading, preparation steps, and target the handoff keeps showing. */
  startup?: Pick<WorkspaceStartup, "heading" | "preparation" | "target">;
  /**
   * Keep the prompt in the workspace composer when the start fails. True for
   * words the reader typed. False for a generated prompt landing in a
   * workspace whose composer already belongs to another conversation.
   */
  holdPromptOnFailure?: boolean;
  /** Open the workspace before its session starts. */
  reveal?: () => Promise<void>;
  onSessionCreated?: (
    session: CodeSessionSnapshot,
    postedModel: string | undefined,
  ) => void;
}): Promise<CodeSessionSnapshot | null> {
  const { client, workspace, settings } = input;
  const prompt = input.prompt.trim();
  const images = input.images ?? [];
  const base: Omit<WorkspaceStartup, "phase" | "hasFirstMessage"> = {
    ...input.startup,
    harness: settings.harness,
  };
  const setWorkspaceStartup = useCodeUiStore.getState().setWorkspaceStartup;
  const hold = input.holdPromptOnFailure ?? true;
  const holdPrompt = () => {
    if (prompt && hold) {
      useCodeUiStore
        .getState()
        .offerComposerPrompt(workspace.id, prompt, images);
    }
  };
  // The copy has to match what the reader just watched happen.
  const created = input.startup?.target !== "this_workspace";
  const sessionFailed = created
    ? "Workspace created, but the session could not start."
    : "The session could not start.";
  const turnFailed =
    "Session started, but the first message could not be sent.";
  const retry = hold
    ? "Send it from the workspace composer."
    : "Try again from the workspace menu.";
  setWorkspaceStartup(workspace.id, {
    ...base,
    hasFirstMessage: Boolean(prompt),
    phase: "starting_session",
  });
  try {
    await input.reveal?.();
    const posted = await resolveSessionModel({
      client,
      harness: settings.harness,
      requested: settings.model,
      models: input.models,
      defaultModelKey: input.defaultModelKey,
    });
    const session = await client.createCodeSession(workspace.id, {
      harness: settings.harness,
      permission_mode: settings.permissionMode,
      model: posted,
      ...(settings.reasoningEffort
        ? { reasoning_effort: settings.reasoningEffort }
        : {}),
      ...(settings.fastMode ? { fast_mode: true } : {}),
    });
    useCodeCatalogStore.getState().rememberSession(session);
    if (prompt) {
      setWorkspaceStartup(workspace.id, {
        ...base,
        hasFirstMessage: true,
        phase: "sending_message",
      });
    }
    input.onSessionCreated?.(session, posted);
    if (prompt) {
      try {
        const attachments = await publishFirstTurnImages(
          client,
          session.id,
          images,
        );
        if (attachments.length > 0) {
          await client.submitCodeTurn(
            session.id,
            prompt,
            undefined,
            attachments,
          );
        } else {
          await client.submitCodeTurn(session.id, prompt);
        }
      } catch (error) {
        // Never drop typed words or pasted images: the workspace composer
        // holds them.
        holdPrompt();
        toast.error(`${turnFailed} ${friendlyErrorMessage(error, retry)}`);
      }
    }
    return session;
  } catch (error) {
    // No session to send to; the workspace composer holds the text, images,
    // and start-session on the workspace page picks them up.
    holdPrompt();
    toast.error(
      `${sessionFailed} ${friendlyErrorMessage(error, hold ? "Try again from the workspace." : retry)}`,
    );
    return null;
  } finally {
    setWorkspaceStartup(workspace.id, null);
  }
}

async function publishFirstTurnImages(
  client: ApiClient,
  sessionId: string,
  files: readonly File[],
): Promise<readonly { blob_id: string; media_type: string }[]> {
  const published = await Promise.all(
    files.map(async (file) => {
      if (hasLocalHostAuthority()) {
        return publishCodeImage(sessionId, file);
      }
      return uploadImageAttachment(client, sessionId, file, {
        onProgress: () => undefined,
        signal: new AbortController().signal,
        path: (id) =>
          `/code/sessions/${encodeURIComponent(id)}/attachments/images`,
      });
    }),
  );
  return published.map((image) => ({
    blob_id: image.attachmentId,
    media_type: image.mediaType,
  }));
}
