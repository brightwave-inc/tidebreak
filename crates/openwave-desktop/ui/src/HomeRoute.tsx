import { useState } from "react";
import { useNavigate } from "@tanstack/react-router";

import { useApp } from "./AppContext";
import { ChatExplorer } from "./ChatExplorer";
import { useChatListStore } from "./ChatListStore";
import { Composer } from "./Composer";
import { useFirstMessage } from "./FirstMessage";
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
  const { client } = useApp();
  const chatsLoaded = useChatListStore((state) => state.chatsLoaded);
  const creatingChat = useChatListStore((state) => state.creatingChat);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);

  async function startChat() {
    const content = draft.trim();
    if (!content || creatingChat) return;
    chatListActions.setCreatingChat(true);
    setError(null);
    try {
      const created = await client.createChat();
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
      <div className="content-container flex min-h-0 w-full min-w-0 flex-1 flex-col overflow-hidden">
        <div className="flex min-h-0 flex-1 flex-col overflow-y-auto px-[clamp(0.5rem,4%,5rem)]">
          <div className="mx-auto flex w-full min-h-0 max-w-3xl flex-1 flex-col gap-8 py-10">
            <div className="space-y-2 text-center">
              <p className="text-3xl font-normal text-foreground">
                What are we working on?
              </p>
              <p className="text-muted-foreground">
                Start a chat, or pick up where you left off.
              </p>
            </div>

            {/* The composer leads, because starting something is what this
                page is for; the list below is for returning to something. */}
            <div>
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
                onDraftChange={setDraft}
                onSend={startChat}
                onSteer={async () => {}}
                onStop={async () => {}}
              />
            </div>

            {chatsLoaded && <ChatExplorer />}
          </div>
        </div>
      </div>
    </RouteFrame>
  );
}
