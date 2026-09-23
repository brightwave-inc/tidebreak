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
import { friendlyErrorMessage } from "@/lib/utils";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeUiStore, type WorkspaceStartup } from "./CodeUiStore";
import {
  gatewayCodeModels,
  preferredCodeModels,
  requiresHarnessModelIds,
} from "./labels";
import {
  seedCodeComposer,
  sendCodeComposer,
  sendCodeTurn,
  useCodeComposerStatus,
} from "./CodeSessionSend";
import { confirmRepositoryTrust } from "./RepositoryTrustStore";

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
 * Start a workspace's first agent and, when there is one, send its first
 * message.
 *
 * The new-workspace dialog and Uneff me both end here. The handoff is keyed
 * by the workspace, so `reveal` runs first: the page the reader lands on is
 * the one carrying the steps.
 *
 * The handoff lasts until the session exists. The message and images then go
 * on that session's composer and leave through its own send, the path every
 * later message takes: the images publish to the new session with their
 * upload status on the chips, and a refused send leaves the words and images
 * in that composer to retry. When no session starts, the workspace's start
 * composer holds them instead, so typed words and pasted images are never
 * dropped.
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
   * Keep the prompt in the workspace's start composer when no session could
   * start. True for words the reader typed. False for a generated prompt that
   * would land in a composer still belonging to another conversation. Once a
   * session exists its composer is the empty one the reader now sees, so a
   * refused first message always waits there.
   */
  holdPromptWithoutSession?: boolean;
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
  const setWorkspaceStartup = useCodeUiStore.getState().setWorkspaceStartup;
  const holdWithoutSession = input.holdPromptWithoutSession ?? true;
  // The copy has to match what the reader just watched happen.
  const created = input.startup?.target !== "this_workspace";
  const sessionFailed = created
    ? "Workspace created, but the session could not start."
    : "The session could not start.";
  setWorkspaceStartup(workspace.id, {
    ...input.startup,
    harness: settings.harness,
    hasFirstMessage: Boolean(prompt),
    phase: "starting_session",
  });
  let session: CodeSessionSnapshot;
  let posted: string | undefined;
  try {
    await input.reveal?.();
    posted = await resolveSessionModel({
      client,
      harness: settings.harness,
      requested: settings.model,
      models: input.models,
      defaultModelKey: input.defaultModelKey,
    });
    // A repository's own engine config can run commands the moment the
    // engine starts, so its first session asks before loading it.
    await confirmRepositoryTrust({
      client,
      workspace,
      harness: settings.harness,
    });
    session = await client.createCodeSession(workspace.id, {
      harness: settings.harness,
      permission_mode: settings.permissionMode,
      model: posted,
      ...(settings.reasoningEffort
        ? { reasoning_effort: settings.reasoningEffort }
        : {}),
      ...(settings.fastMode ? { fast_mode: true } : {}),
    });
  } catch (error) {
    // No session to send to; the workspace's start composer holds the text
    // and images, and starting a session there sends them.
    if (holdWithoutSession && prompt) {
      seedCodeComposer(workspace.id, prompt, images);
    }
    toast.error(
      `${sessionFailed} ${friendlyErrorMessage(error, holdWithoutSession ? "Try again from the workspace." : "Try again from the workspace menu.")}`,
    );
    return null;
  } finally {
    // The conversation takes over as soon as the session exists.
    setWorkspaceStartup(workspace.id, null);
  }
  useCodeCatalogStore.getState().rememberSession(session);
  input.onSessionCreated?.(session, posted);
  if (!prompt) return session;
  seedCodeComposer(session.id, prompt, images);
  const sent = await sendCodeComposer({
    client,
    key: session.id,
    session: session.id,
    send: (sessionId, message, attachments) =>
      sendCodeTurn({ client, sessionId, message, attachments }),
  });
  if (!sent) {
    // The reader may be looking at another workspace, so say it here too.
    const reason = useCodeComposerStatus.getState().byKey[session.id]?.notice;
    toast.error(
      `Session started, but the first message could not be sent. ${reason ?? "Send it from the workspace composer."}`,
    );
  }
  return session;
}
