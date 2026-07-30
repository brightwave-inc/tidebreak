import { useState } from "react";
import { useNavigate } from "@tanstack/react-router";

import { useApp } from "./AppContext";
import { attachChatFiles } from "./attachments";
import { useChatListStore } from "./ChatListStore";
import { Composer, type ComposerImages } from "./Composer";
import {
  HOME_DRAFT_KEY,
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
import { useNewChatSettings } from "./NewChatSettings";
import { PermissionModeMenu } from "./PermissionModeMenu";
import { RouteFrame } from "./RouteFrame";
import { HomeSidebar } from "./sidebar/HomeSidebar";
import { WelcomeState } from "./WelcomeState";
import type { AttachedFiles } from "./attachments";
import { MAX_IMAGE_ATTACHMENTS } from "./ImageAttachments";

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
  const [error, setError] = useState<string | null>(null);
  const newChat = useNewChatSettings();
  const efforts = modelForSelection(models, newChat.model)?.reasoning_efforts ?? [];

  // A chat created silently when the user attaches files before typing. The
  // chat exists on the server so files can upload, but the user stays on the
  // home page until they send.
  const [pendingChatId, setPendingChatId] = useState<string | null>(null);
  const [pendingImages, setPendingImages] = useState<ImageAttachment[]>([]);
  const [pendingFiles, setPendingFiles] = useState<ImportedDocument[]>([]);
  const [attaching, setAttaching] = useState(false);
  const [attachError, setAttachError] = useState<string | null>(null);

  async function ensurePendingChat(): Promise<string> {
    if (pendingChatId) return pendingChatId;
    const created = await client.createChat(newChat.model ?? undefined, null, {
      reasoningEffort: newChat.reasoningEffort,
      permissionMode: newChat.permissionMode,
    });
    chatListActions.prependChat(created);
    chatListActions.setChatsError(null);
    setPendingChatId(created.id);
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
      setDraft("");
      setPendingChatId(null);
      setPendingImages([]);
      setPendingFiles([]);
      setAttachError(null);
      await navigate({ to: "/c/$chatId", params: { chatId } });
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

  return (
    <RouteFrame sidebar={<HomeSidebar />}>
      <div className="content-container flex min-h-0 w-full min-w-0 flex-1 flex-col overflow-hidden px-[clamp(0.5rem,4%,5rem)]">
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
                  value={newChat.model}
                  defaultKey={defaultModelKey}
                  disabled={creatingChat}
                  onChange={newChat.setModel}
                />
                {efforts.length > 0 && (
                  <ReasoningEffortMenu
                    levels={efforts}
                    value={newChat.reasoningEffort}
                    disabled={creatingChat}
                    onChange={newChat.setReasoningEffort}
                  />
                )}
                <PermissionModeMenu
                  value={newChat.permissionMode}
                  disabled={creatingChat}
                  onChange={newChat.setPermissionMode}
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
    </RouteFrame>
  );
}
