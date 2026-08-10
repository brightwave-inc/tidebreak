import { useEffect, useMemo, useState } from "react";
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
import {
  readyImageAttachment,
  type ImageAttachment,
  type PickedImage,
} from "./ImageAttachments";
import { ModelMenu, useModelSettingsNav } from "./ModelMenu";
import { modelForSelection } from "./ModelSelection";
import { effectiveNewChatSettings, useNewChatSettings } from "./NewChatSettings";
import { PermissionModeMenu } from "./PermissionModeMenu";
import { pluginsApisFromClient } from "./plugins/pluginsApis";
import { useComposerPlugins } from "./plugins/useComposerPlugins";
import { RouteFrame } from "./RouteFrame";
import { AppSidebar } from "./sidebar/AppSidebar";
import { WelcomeState } from "./WelcomeState";
import type { AttachedFiles } from "./attachments";
import { MAX_IMAGE_ATTACHMENTS } from "./ImageAttachments";
import { appendTranscript, useVoiceComposer } from "./useVoiceComposer";
import { useVoiceInputStore, voiceSelectionReady } from "./VoiceInputStore";

const chatListActions = useChatListStore.getState();
const composerDraftActions = useComposerDrafts.getState();
const firstMessageActions = useFirstMessage.getState();

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

  function setPendingImages(
    update: (current: readonly ImageAttachment[]) => ImageAttachment[],
  ) {
    const current =
      useComposerDrafts.getState().attachments[HOME_DRAFT_KEY]?.images ?? [];
    composerDraftActions.setImages(HOME_DRAFT_KEY, update(current));
  }

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
      setPendingImages((current) => [
        ...current,
        ...pickedImages.map((image: PickedImage) =>
          readyImageAttachment(crypto.randomUUID(), image),
        ),
      ]);
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

  const composerImages: ComposerImages | undefined =
    pendingImages.length > 0
      ? {
          items: pendingImages,
          error: null,
          unsupportedModel: null,
          onAttachFiles: () => {},
          onRemove: (id) =>
            setPendingImages((prev) => prev.filter((img) => img.id !== id)),
          onRetry: () => {},
        }
      : undefined;

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
              pendingChatId ? (
                <DocumentDropTarget
                  chatId={pendingChatId}
                  onAttached={adoptAttached}
                  onError={(caught) =>
                    setAttachError(
                      String(caught).replace(/^Error:\s*/, "").trim() ||
                        "Could not attach that file.",
                    )
                  }
                />
              ) : undefined
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
