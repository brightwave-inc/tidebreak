import { useState } from "react";
import { useNavigate } from "@tanstack/react-router";

import { useApp } from "./AppContext";
import { ChatExplorer } from "./ChatExplorer";
import { useChatListStore } from "./ChatListStore";
import { Composer } from "./Composer";
import { useFirstMessage } from "./FirstMessage";
import { ModelMenu, ReasoningEffortMenu } from "./ModelMenu";
import { modelForSelection } from "./ModelSelection";
import { useNewChatSettings } from "./NewChatSettings";
import { PermissionModeMenu } from "./PermissionModeMenu";
import { RouteFrame } from "./RouteFrame";
import { HomeSidebar } from "./sidebar/HomeSidebar";

const chatListActions = useChatListStore.getState();
const firstMessageActions = useFirstMessage.getState();

/**
 * Where the app opens, and where the logo goes back to.
 *
 * The composer here starts a conversation rather than posting into one. It
 * hands the text to the chat it creates instead of sending it directly, so
 * there is still exactly one send path — see [useFirstMessage].
 *
 * Nothing on this route is scoped to a conversation, including its rail. A
 * conversation's sources, outputs and folders are reachable from inside one,
 * which is the only place they describe anything.
 */
export function HomeRoute() {
  const navigate = useNavigate();
  const { client, models, defaultModelKey } = useApp();
  const chatsLoaded = useChatListStore((state) => state.chatsLoaded);
  const creatingChat = useChatListStore((state) => state.creatingChat);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  const newChat = useNewChatSettings();
  // Only the levels the pending model accepts are offerable, the same rule the
  // conversation composer follows.
  const efforts = modelForSelection(models, newChat.model)?.reasoning_efforts ?? [];

  async function startChat() {
    const content = draft.trim();
    if (!content || creatingChat) return;
    chatListActions.setCreatingChat(true);
    setError(null);
    try {
      const created = await client.createChat(newChat.model ?? undefined, null, {
        reasoningEffort: newChat.reasoningEffort,
        permissionMode: newChat.permissionMode,
      });
      chatListActions.prependChat(created);
      chatListActions.setChatsError(null);
      firstMessageActions.hold(created.id, content);
      setDraft("");
      await navigate({ to: "/c/$chatId", params: { chatId: created.id } });
    } catch (err) {
      setError(`Could not start a chat: ${String(err)}`);
    } finally {
      chatListActions.setCreatingChat(false);
    }
  }

  return (
    <RouteFrame sidebar={<HomeSidebar />}>
      <div className="content-container flex min-h-0 w-full min-w-0 flex-1 flex-col overflow-hidden px-[clamp(0.5rem,4%,5rem)]">
        {/* The greeting and the list of past chats scroll together and stay
            centred while there is room; the composer below does not move. */}
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

        {/* Docked at the foot of the page: the composer here starts a chat
            rather than posting into one, but it stays put while the list above
            scrolls, the way it does inside a conversation. */}
        <div className="z-10 mx-auto w-full max-w-3xl pb-2">
          {error && <p className="pb-2 text-sm text-critical">{error}</p>}
          <Composer
            activeTurnId={null}
            busy={false}
            cancelError={null}
            cancelPending={false}
            disabled={creatingChat}
            draft={draft}
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
