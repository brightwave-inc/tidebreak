import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";

import { useApp } from "./AppContext";
import { attachChatFiles } from "./attachments";
import { useChatListStore } from "./ChatListStore";
import { Composer, type ComposerImages } from "./Composer";
import {
  HOME_DRAFT_KEY,
  useComposerAttachments,
  useComposerDraft,
  useComposerDrafts,
} from "./ComposerDrafts";
import {
  type ImportedDocument,
  type LibraryImportSuccess,
} from "./documents";
import { DocumentDropTarget } from "./DocumentDropTarget";
import { useFirstMessage } from "./FirstMessage";
import { hasNativeHost } from "./host";
import { ModelMenu, useModelSettingsNav } from "./ModelMenu";
import { modelForSelection, textOnlyModelLabel } from "./ModelSelection";
import { effectiveNewChatSettings, useNewChatSettings } from "./NewChatSettings";
import { PermissionModeMenu } from "./PermissionModeMenu";
import { pluginsApisFromClient } from "./plugins/pluginsApis";
import { useComposerPlugins } from "./plugins/useComposerPlugins";
import { RouteFrame } from "./RouteFrame";
import { AppSidebar } from "./sidebar/AppSidebar";
import { WelcomeState } from "./WelcomeState";
import type { AttachedFiles } from "./attachments";
import { MAX_IMAGE_ATTACHMENTS } from "./ImageAttachments";
import { useImageAttachments } from "./useImageAttachments";
import { appendTranscript, useVoiceComposer } from "./useVoiceComposer";
import { useVoiceInputStore, voiceSelectionReady } from "./VoiceInputStore";

const chatListActions = useChatListStore.getState();
const composerDraftActions = useComposerDrafts.getState();
const firstMessageActions = useFirstMessage.getState();

/**
 * A file picker creates a chat before there is a first message. Reconcile the
 * home pickers immediately before that first message is held so changes made
 * while the attachment strip was open govern the turn that follows.
 *
 * That includes the model and its reasoning level: the chat was created with
 * whatever was selected when the attachment opened it, and a reader who picks
 * a different model before sending expects the turn to run on the one they can
 * see in the composer.
 *
 * A null model is not sent, and the reasoning level rides with it. It means
 * the home picker never resolved one — loading the defaults is allowed to fail
 * quietly — and the chat the server seeded from the sticky default is a better
 * answer than the global default that clearing it would fall back to.
 */
export async function applyPendingChatSettings(
  client: Pick<
    typeof import("./api").ApiClient.prototype,
    | "patchChatModel"
    | "patchChatReasoningEffort"
    | "patchChatPermissionMode"
    | "patchChatNetworkPolicy"
  >,
  chatId: string,
  settings: {
    model: import("./api").ModelSelectionKey | null;
    reasoningEffort: import("./api").ReasoningEffort | null;
    permissionMode: import("./api").PermissionMode | null;
    networkPolicy: import("./api").NetworkPolicy;
  },
): Promise<void> {
  if (settings.model) {
    chatListActions.replaceChat(
      await client.patchChatModel(chatId, settings.model),
    );
    chatListActions.replaceChat(
      await client.patchChatReasoningEffort(chatId, settings.reasoningEffort),
    );
  }
  chatListActions.replaceChat(
    await client.patchChatPermissionMode(chatId, settings.permissionMode),
  );
  chatListActions.replaceChat(
    await client.patchChatNetworkPolicy(chatId, settings.networkPolicy),
  );
}

/**
 * One in-flight run shared by everyone who asks for it while it is running.
 *
 * Home creates its pending chat lazily, and the callers race: a dropped batch
 * of three images asks for the chat three times in the same tick, and the
 * paperclip can be mid-flight when a paste lands. Without this each caller
 * creates its own chat and the reader is left with orphans in the sidebar.
 * The run is forgotten as soon as it settles, so a failed creation is retried
 * rather than remembered.
 */
export function singleFlight<T>(): (run: () => Promise<T>) => Promise<T> {
  let inFlight: Promise<T> | null = null;
  return (run) => {
    if (!inFlight) {
      inFlight = run().finally(() => {
        inFlight = null;
      });
    }
    return inFlight;
  };
}

function isImportedDocument(
  result: { status: string },
): result is LibraryImportSuccess {
  return result.status === "imported" || result.status === "already_present";
}

export function HomeRoute() {
  const navigate = useNavigate();
  const { client, models, defaultModelKey, providers, modelVisibilityOverrides } =
    useApp();
  const modelSettingsNav = useModelSettingsNav();
  const creatingChat = useChatListStore((state) => state.creatingChat);
  const draft = useComposerDraft(HOME_DRAFT_KEY);
  const composerPlugins = useComposerPlugins(client);
  const promptLibrary = useMemo(() => pluginsApisFromClient(client), [client]);
  const setDraft = (text: string) =>
    composerDraftActions.setDraft(HOME_DRAFT_KEY, text);
  const voice = useVoiceComposer(
    (audio) => client.transcribeVoice(audio),
    (transcript) => {
      const current = useComposerDrafts.getState().drafts[HOME_DRAFT_KEY] ?? "";
      setDraft(appendTranscript(current, transcript));
    },
    undefined,
    async () => {
      const info = await useVoiceInputStore.getState().load(client);
      if (voiceSelectionReady(info)) return true;
      const path: string = "/settings/voice-transcription";
      await navigate({ to: path });
      return false;
    },
  );
  const [error, setError] = useState<string | null>(null);
  const newChat = useNewChatSettings();
  // What the pickers show and the created chat will get: this visit's picks
  // over the server's sticky defaults. Only the explicit picks are sent; the
  // server seeds the rest from the same defaults being displayed.
  const effective = effectiveNewChatSettings(newChat);
  const efforts = modelForSelection(models, effective.model)?.reasoning_efforts ?? [];

  // A choice made inside a chat is recorded server-side as the sticky
  // default; re-read it whenever the reader lands back here so the pickers
  // show what the next chat will actually start with.
  useEffect(() => {
    void useNewChatSettings.getState().loadDefaults(client);
  }, [client]);

  // A chat created silently when the user attaches files before typing. The
  // chat exists on the server so files can upload, but the user stays on the
  // home page until they send. The id is part of the home draft: without it a
  // restored attachment strip would publish to a chat nobody remembers.
  const attachments = useComposerAttachments(HOME_DRAFT_KEY);
  const pendingChatId = attachments.pendingChatId;
  const pendingImages = attachments.images;
  const pendingFiles = attachments.files;
  const pendingSkills = attachments.skills;
  const [attaching, setAttaching] = useState(false);
  const [attachError, setAttachError] = useState<string | null>(null);
  // One chat creation shared by every route into it — the picker, and each file
  // of a dropped or pasted batch.
  const creatingPendingChat = useRef(singleFlight<string>());
  // The same strip a conversation's composer has. Home's bytes are published
  // into the chat the attachment silently creates, which is why the target is
  // resolved per upload rather than being the draft's own key.
  const images = useImageAttachments(client, HOME_DRAFT_KEY, ensurePendingChat);

  const chats = useChatListStore((state) => state.chats);
  const chatsLoaded = useChatListStore((state) => state.chatsLoaded);

  // A restored home draft may point at a chat that no longer exists — deleted
  // in another window since the attachments were published to it. Those images
  // and files can never send, so drop them; the text stands on its own.
  useEffect(() => {
    if (!chatsLoaded || !pendingChatId) return;
    if (chats.some((chat) => chat.id === pendingChatId)) return;
    composerDraftActions.setPendingChatId(HOME_DRAFT_KEY, null);
    composerDraftActions.setImages(HOME_DRAFT_KEY, []);
    composerDraftActions.setFiles(HOME_DRAFT_KEY, []);
  }, [chatsLoaded, chats, pendingChatId]);

  function setPendingFiles(
    update: (current: readonly ImportedDocument[]) => ImportedDocument[],
  ) {
    const current =
      useComposerDrafts.getState().attachments[HOME_DRAFT_KEY]?.files ?? [];
    composerDraftActions.setFiles(HOME_DRAFT_KEY, update(current));
  }

  async function ensurePendingChat(): Promise<string> {
    const existing =
      useComposerDrafts.getState().attachments[HOME_DRAFT_KEY]?.pendingChatId;
    if (existing) return existing;
    // Images arrive in batches — a multi-file drop uploads every file at once,
    // and each upload asks for the chat to publish into. One in-flight creation
    // is shared between them so a three-image drop does not leave two orphan
    // chats behind it.
    return creatingPendingChat.current(createPendingChat);
  }

  async function createPendingChat(): Promise<string> {
    const created = await client.createChat(newChat.model ?? undefined, null, {
      reasoningEffort: newChat.reasoningEffort,
      permissionMode: newChat.permissionMode,
      networkPolicy: newChat.networkPolicy ?? undefined,
    });
    chatListActions.prependChat(created);
    chatListActions.setChatsError(null);
    composerDraftActions.setPendingChatId(HOME_DRAFT_KEY, created.id);
    return created.id;
  }

  async function onAttach() {
    if (attaching || creatingChat) return;
    setAttaching(true);
    setAttachError(null);
    try {
      const chatId = await ensurePendingChat();
      const attached = await attachChatFiles(chatId);
      if (!attached) return;
      adoptAttached(attached);
    } catch (err) {
      setAttachError(
        String(err).replace(/^Error:\s*/, "").trim() ||
          "Could not attach that file.",
      );
    } finally {
      setAttaching(false);
    }
  }

  function adoptAttached(attached: AttachedFiles) {
    const seenDocumentIds = new Set(
      pendingFiles.map((file) => file.documentId),
    );
    const imported =
      attached.documents?.results
        .filter(isImportedDocument)
        .map((result) => result.document)
        .filter((document) => {
          if (seenDocumentIds.has(document.documentId)) return false;
          seenDocumentIds.add(document.documentId);
          return true;
        }) ?? [];
    const remaining =
      MAX_IMAGE_ATTACHMENTS - pendingImages.length - pendingFiles.length;
    const pickedImages = attached.images.slice(0, Math.max(0, remaining));
    const pickedFiles = imported.slice(
      0,
      Math.max(0, remaining - pickedImages.length),
    );
    if (pickedImages.length > 0) {
      images.adopt(pickedImages);
    }
    if (pickedFiles.length > 0) {
      setPendingFiles((current) => [...current, ...pickedFiles]);
    }
    if (
      pickedImages.length + pickedFiles.length <
      attached.images.length + imported.length
    ) {
      setAttachError(
        `A message can carry at most ${MAX_IMAGE_ATTACHMENTS} attachments.`,
      );
    }
    const failedDocument = attached.documents?.results.find(
      (result) => result.status === "failed",
    );
    if (failedDocument?.status === "failed") {
      setAttachError(`${failedDocument.displayName}: ${failedDocument.message}`);
    }
    const [failedImage] = attached.failedImages;
    if (failedImage) {
      setAttachError(`${failedImage.fileName}: ${failedImage.message}`);
    }
  }

  async function startChat() {
    const content = draft.trim();
    if (!content || creatingChat) return;
    chatListActions.setCreatingChat(true);
    setError(null);
    try {
      // Reuse the chat that was silently created for file attachments, or
      // create a fresh one.
      let chatId = pendingChatId;
      if (!chatId) {
        const created = await client.createChat(
          newChat.model ?? undefined,
          null,
          {
            reasoningEffort: newChat.reasoningEffort,
            permissionMode: newChat.permissionMode,
            networkPolicy: newChat.networkPolicy ?? undefined,
          },
        );
        chatListActions.prependChat(created);
        chatListActions.setChatsError(null);
        chatId = created.id;
      } else {
        await applyPendingChatSettings(client, chatId, {
          model: effective.model,
          reasoningEffort: effective.reasoningEffort,
          permissionMode: effective.permissionMode,
          networkPolicy: effective.networkPolicy,
        });
      }
      firstMessageActions.hold(chatId, {
        text: content,
        images: pendingImages,
        files: pendingFiles,
        skills: pendingSkills,
        voiceInputUsed: voice.inputUsed,
      });
      // Clear the home draft only once navigation has committed. If it throws,
      // the message lives only in the FirstMessage store with no composer
      // showing it — the draft has to stay where the reader can see and
      // resend it.
      await navigate({ to: "/c/$chatId", params: { chatId } });
      composerDraftActions.clearDraft(HOME_DRAFT_KEY);
    } catch (err) {
      setError(`Could not start a chat: ${String(err)}`);
    } finally {
      chatListActions.setCreatingChat(false);
    }
  }

  // Offered whether or not anything is attached yet: a drop or a paste is how
  // the first image usually arrives, and the composer only claims one when a
  // strip is there to receive it.
  const composerImages: ComposerImages = {
    items: pendingImages,
    error: images.error,
    unsupportedModel: textOnlyModelLabel(models, effective.model),
    onAttachFiles: (selected) => {
      if (
        pendingImages.length + pendingFiles.length + selected.length >
        MAX_IMAGE_ATTACHMENTS
      ) {
        setAttachError(
          `A message can carry at most ${MAX_IMAGE_ATTACHMENTS} attachments.`,
        );
        return;
      }
      setAttachError(null);
      images.attachFiles(selected);
    },
    onRemove: images.remove,
    onRetry: images.retry,
  };

  // Home is the composer alone. The install-wide libraries that used to open
  // as panels here are routes of their own now, so nothing beside the
  // conversation starter needs hosting.
  return (
    <RouteFrame sidebar={<AppSidebar />}>
      <div className="content-container flex min-h-0 w-full min-w-0 flex-1 flex-col overflow-hidden">
        {/* The panel slot this used to sit in was a plain block, so nothing
            stretches the column to the slot's height — it has to claim it
            itself, the same way .chat-pane does. */}
        <div className="flex h-full min-h-0 w-full min-w-0 flex-col overflow-hidden px-[clamp(0.5rem,4%,5rem)]">
        <div className="flex min-h-0 flex-1 items-center justify-center overflow-y-auto">
          {/* The same null state an empty conversation shows: home is where a
              chat starts, so it greets the same way. Picking a starter prompt
              fills the composer rather than sending, the way it does in a chat.
              Home's starters come from the installed prompt library when it has
              any; otherwise the built-in openers stand. */}
          <WelcomeState
            onSelectPrompt={(prompt) => {
              setDraft(prompt);
              voice.resetInputUsed();
            }}
            executionConfigClient={client}
            promptLibrary={promptLibrary}
          />
        </div>

        <div className="z-10 mx-auto w-full max-w-3xl pb-2">
          {error && <p className="pb-2 text-sm text-critical">{error}</p>}
          <Composer
            activeTurnId={null}
            busy={false}
            cancelError={null}
            cancelPending={false}
            disabled={creatingChat}
            draft={draft}
            plugins={composerPlugins.plugins}
            slash={{
              options: composerPlugins.slashOptions,
              invoked: pendingSkills,
              onInvoke: (names) =>
                composerDraftActions.setSkills(HOME_DRAFT_KEY, [
                  ...pendingSkills,
                  ...names,
                ]),
              onRemove: (name) =>
                composerDraftActions.setSkills(
                  HOME_DRAFT_KEY,
                  pendingSkills.filter((skill) => skill !== name),
                ),
              loadPromptBody: composerPlugins.loadPromptBody,
            }}
            images={composerImages}
            voice={{
              available: voice.available,
              state: voice.state,
              error: voice.error,
              onStart: () => void voice.start(),
              onStop: voice.stop,
            }}
            files={{
              items: pendingFiles,
              attaching,
              onAttach: hasNativeHost() ? onAttach : undefined,
              onRemove: (documentId) =>
                setPendingFiles((current) =>
                  current.filter((file) => file.documentId !== documentId),
                ),
            }}
            nativeDropTarget={
              <DocumentDropTarget
                resolveChatId={ensurePendingChat}
                onAttached={adoptAttached}
                onError={(caught) =>
                  setAttachError(
                    String(caught).replace(/^Error:\s*/, "").trim() ||
                      "Could not attach that file.",
                  )
                }
              />
            }
            attachError={attachError}
            resetKey="home"
            steerError={null}
            steerPending={false}
            steerStatus={null}
            modelMenu={
              <ModelMenu
                models={models}
                value={effective.model}
                defaultKey={defaultModelKey}
                disabled={creatingChat}
                visibilityOverrides={modelVisibilityOverrides}
                providers={providers}
                onManageModels={modelSettingsNav.onManageModels}
                onSetUpProvider={modelSettingsNav.onSetUpProvider}
                onChange={newChat.setModel}
              />
            }
            permissionMenu={
              <PermissionModeMenu
                scopeKey="new-chat"
                value={effective.permissionMode}
                disabled={creatingChat}
                onChange={newChat.setPermissionMode}
              />
            }
            reasoning={{
              levels: efforts,
              value: effective.reasoningEffort,
              disabled: creatingChat,
              onChange: newChat.setReasoningEffort,
            }}
            network={{
              value: effective.networkPolicy,
              disabled: creatingChat,
              onChange: newChat.setNetworkPolicy,
            }}
            onDraftChange={(next) => {
              setDraft(next);
              if (!next.trim()) voice.resetInputUsed();
            }}
            onSend={startChat}
            onSteer={async () => {}}
            onStop={async () => {}}
          />
        </div>
        </div>
      </div>
    </RouteFrame>
  );
}
