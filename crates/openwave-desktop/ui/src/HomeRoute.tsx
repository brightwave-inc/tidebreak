import { useEffect, useState } from "react";
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
import { ModelMenu, ReasoningEffortMenu } from "./ModelMenu";
import { modelForSelection } from "./ModelSelection";
import { effectiveNewChatSettings, useNewChatSettings } from "./NewChatSettings";
import { AppsPanel } from "./apps/AppsPanel";
import { PanelLayout } from "./panel/PanelLayout";
import type { LayoutState, PanelContent } from "./panel/panelTypes";
import { useLayoutState } from "./panel/usePanelNav";
import { NetworkPolicyMenu } from "./NetworkPolicyMenu";
import { PermissionModeMenu } from "./PermissionModeMenu";
import { RouteFrame } from "./RouteFrame";
import { HomeSidebar } from "./sidebar/HomeSidebar";
import { WelcomeState } from "./WelcomeState";
import type { AttachedFiles } from "./attachments";
import { MAX_IMAGE_ATTACHMENTS } from "./ImageAttachments";
import { appendTranscript, useVoiceComposer } from "./useVoiceComposer";

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
  const { client, models, defaultModelKey } = useApp();
  const creatingChat = useChatListStore((state) => state.creatingChat);
  const draft = useComposerDraft(HOME_DRAFT_KEY);
  const setDraft = (text: string) =>
    composerDraftActions.setDraft(HOME_DRAFT_KEY, text);
  const voice = useVoiceComposer((audio) => client.transcribeVoice(audio), (transcript) => {
    const current = useComposerDrafts.getState().drafts[HOME_DRAFT_KEY] ?? "";
    setDraft(appendTranscript(current, transcript));
  });
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

  // Home hosts panels the way a conversation does, but only the ones that
  // mean something outside a chat — today, the Apps library. Anything else in
  // the URL collapses back to home alone rather than rendering a panel whose
  // content is scoped to a conversation this route does not have.
  const layout = homeLayout(useLayoutState());

  function renderPanel(
    panel: PanelContent,
    position: "left" | "right" | "chat",
  ) {
    if (panel.type === "apps" && position !== "chat") {
      return <AppsPanel panel={panel} position={position} />;
    }
    return homeContent();
  }

  function homeContent() {
    return (
      // The panel slot this sits in is a plain block, so nothing stretches
      // the column to the slot's height — it has to claim it itself, the
      // same way .chat-pane does.
      <div className="flex h-full min-h-0 w-full min-w-0 flex-col overflow-hidden px-[clamp(0.5rem,4%,5rem)]">
        <div className="flex min-h-0 flex-1 items-center justify-center overflow-y-auto">
          {/* The same null state an empty conversation shows: home is where a
              chat starts, so it greets the same way. Picking a starter prompt
              fills the composer rather than sending, the way it does in a chat. */}
          <WelcomeState onSelectPrompt={setDraft} />
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
              <>
                <ModelMenu
                  models={models}
                  value={effective.model}
                  defaultKey={defaultModelKey}
                  disabled={creatingChat}
                  onChange={newChat.setModel}
                />
                {efforts.length > 0 && (
                  <ReasoningEffortMenu
                    levels={efforts}
                    value={effective.reasoningEffort}
                    disabled={creatingChat}
                    onChange={newChat.setReasoningEffort}
                  />
                )}
                <PermissionModeMenu
                  value={effective.permissionMode}
                  disabled={creatingChat}
                  onChange={newChat.setPermissionMode}
                />
                <NetworkPolicyMenu
                  value={effective.networkPolicy}
                  disabled={creatingChat}
                  onChange={newChat.setNetworkPolicy}
                />
              </>
            }
            onDraftChange={setDraft}
            onSend={startChat}
            onSteer={async () => {}}
            onStop={async () => {}}
          />
        </div>
      </div>
    );
  }

  return (
    <RouteFrame sidebar={<HomeSidebar />}>
      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        <PanelLayout layout={layout} renderPanel={renderPanel} />
      </div>
    </RouteFrame>
  );
}

/**
 * The layouts home is willing to host: itself alone, or itself beside the
 * Apps library. A URL naming any conversation-scoped panel is a stale or
 * hand-edited link; it lands on plain home rather than an empty panel.
 */
function homeLayout(layout: LayoutState): LayoutState {
  if (layout.mode === "single") return { mode: "single", panel: { type: "chat" } };
  const supported = (panel: PanelContent) =>
    panel.type === "chat" || panel.type === "apps";
  if (!supported(layout.left) || !supported(layout.right)) {
    return { mode: "single", panel: { type: "chat" } };
  }
  return layout;
}
