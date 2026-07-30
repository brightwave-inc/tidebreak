import { useState } from "react";
import { useNavigate } from "@tanstack/react-router";

import { useApp } from "./AppContext";
import { attachChatFiles } from "./attachments";
import { ChatExplorer } from "./ChatExplorer";
import { useChatListStore } from "./ChatListStore";
import { Composer, type ComposerImages } from "./Composer";
import type { LibraryImportSuccess } from "./documents";
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
import { ToolsMenu } from "./ToolsMenu";

const chatListActions = useChatListStore.getState();
const firstMessageActions = useFirstMessage.getState();

function isImportedDocument(
  result: { status: string },
): result is LibraryImportSuccess {
  return result.status === "imported" || result.status === "already_present";
}

export function HomeRoute() {
  const navigate = useNavigate();
  const { client, models, defaultModelKey } = useApp();
  const chatsLoaded = useChatListStore((state) => state.chatsLoaded);
  const creatingChat = useChatListStore((state) => state.creatingChat);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  const newChat = useNewChatSettings();
  const efforts = modelForSelection(models, newChat.model)?.reasoning_efforts ?? [];

  // A chat created silently when the user attaches files before typing. The
  // chat exists on the server so files can upload, but the user stays on the
  // home page until they send.
  const [pendingChatId, setPendingChatId] = useState<string | null>(null);
  const [pendingImages, setPendingImages] = useState<ImageAttachment[]>([]);
  const [pendingSourceName, setPendingSourceName] = useState<string | null>(null);
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
      if (attached.images.length > 0) {
        setPendingImages((prev) => [
          ...prev,
          ...attached.images.map((img: PickedImage) =>
            readyImageAttachment(crypto.randomUUID(), img),
          ),
        ]);
      }
      const source = attached.documents?.results.find(isImportedDocument);
      if (source) setPendingSourceName(source.document.displayName);
      const [firstFailure] = attached.failedImages;
      if (firstFailure) {
        setAttachError(`${firstFailure.fileName}: ${firstFailure.message}`);
      }
    } catch (err) {
      setAttachError(
        String(err).replace(/^Error:\s*/, "").trim() ||
          "Could not attach that file.",
      );
    } finally {
      setAttaching(false);
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
      firstMessageActions.hold(chatId, content);
      setDraft("");
      setPendingChatId(null);
      setPendingImages([]);
      setPendingSourceName(null);
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
          <div className="mx-auto flex w-full max-w-3xl flex-col gap-8 py-10">
            <div className="space-y-2 text-center">
              <p className="text-3xl font-normal text-foreground">
                What are we working on?
              </p>
              <p className="text-muted-foreground">
                Start a chat, or pick up where you left off.
              </p>
            </div>

            {chatsLoaded && <ChatExplorer />}
          </div>
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
            attachedSourceName={pendingSourceName}
            attachError={attachError}
            onDismissAttachedSource={() => setPendingSourceName(null)}
            resetKey="home"
            steerError={null}
            steerPending={false}
            steerStatus={null}
            modelMenu={
              <>
                <ToolsMenu
                  disabled={creatingChat}
                  onAttach={hasNativeHost() ? onAttach : undefined}
                  attaching={attaching}
                />
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
