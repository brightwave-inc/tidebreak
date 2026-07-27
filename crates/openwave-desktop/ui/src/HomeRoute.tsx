import { useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { MessageCircleMore } from "lucide-react";

import { useApp } from "./AppContext";
import { useChatListStore } from "./ChatListStore";
import { Composer } from "./Composer";
import { useFirstMessage } from "./FirstMessage";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "./components/ui/empty";

const chatListActions = useChatListStore.getState();
const firstMessageActions = useFirstMessage.getState();

/** Shown on the rail and here; the Chats panel has the rest. */
const HOME_RECENT_LIMIT = 6;

/**
 * Where the app opens, and where the logo goes back to.
 *
 * The composer here starts a conversation rather than posting into one. It
 * hands the text to the chat it creates instead of sending it directly, so
 * there is still exactly one send path — see [useFirstMessage].
 */
export function HomeRoute() {
  const navigate = useNavigate();
  const { client } = useApp();
  const chats = useChatListStore((state) => state.chats);
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

  const recent = chats.slice(0, HOME_RECENT_LIMIT);

  return (
    <div className="content-container flex min-h-0 w-full min-w-0 flex-1 flex-col overflow-hidden">
      <div className="flex flex-1 items-center justify-center overflow-y-auto px-[clamp(0.5rem,4%,5rem)]">
        <div className="mx-auto flex w-full max-w-3xl flex-col gap-8 py-10">
          <div className="space-y-2 text-center">
            <p className="text-3xl font-normal text-foreground">What are we working on?</p>
            <p className="text-muted-foreground">
              Start a chat, or pick up where you left off.
            </p>
          </div>

          {chatsLoaded && recent.length === 0 ? (
            <Empty className="border">
              <EmptyHeader>
                <EmptyMedia variant="icon">
                  <MessageCircleMore />
                </EmptyMedia>
                <EmptyTitle>No chats yet</EmptyTitle>
                <EmptyDescription>
                  Ask a question below and OpenWave will open your first chat.
                </EmptyDescription>
              </EmptyHeader>
            </Empty>
          ) : (
            recent.length > 0 && (
              <div className="flex flex-col gap-1">
                <p className="px-2 text-sm font-medium text-muted-foreground">Recent</p>
                <ul className="flex flex-col gap-0.5">
                  {recent.map((chat) => (
                    <li key={chat.id}>
                      <button
                        type="button"
                        className="flex w-full cursor-pointer items-center gap-2 rounded-md px-2 py-2 text-left text-sm transition-colors hover:bg-muted"
                        onClick={() =>
                          void navigate({ to: "/c/$chatId", params: { chatId: chat.id } })
                        }
                      >
                        <MessageCircleMore className="size-4 shrink-0 text-muted-foreground" />
                        <span className="min-w-0 truncate">
                          {chat.title?.trim() || "New chat"}
                        </span>
                      </button>
                    </li>
                  ))}
                </ul>
              </div>
            )
          )}
        </div>
      </div>

      <div className="z-10 px-[clamp(0.5rem,4%,5rem)] pb-2">
        <div className="mx-auto w-full max-w-3xl">
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
      </div>
    </div>
  );
}
