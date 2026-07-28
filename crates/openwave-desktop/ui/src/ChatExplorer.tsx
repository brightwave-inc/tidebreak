import { useMemo, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { CircleAlert, MessageCircleMore, Search } from "lucide-react";

import type { Chat } from "./api";
import { useChatAttention } from "./ChatAttention";
import { useChatListStore } from "./ChatListStore";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "./components/ui/empty";
import { Input } from "./components/ui/input";

/** Case-insensitive match on the title, with untitled chats matching "new chat". */
export function matchesChatSearch(chat: Chat, query: string): boolean {
  const trimmed = query.trim().toLowerCase();
  if (!trimmed) return true;
  const title = chat.title?.trim() || "New chat";
  return title.toLowerCase().includes(trimmed);
}

/**
 * Every conversation, searchable.
 *
 * This lives on home rather than beside a conversation. Finding a chat is
 * something you do between conversations, not inside one — offering it from
 * within a chat is what made the rail there a general-purpose list that
 * happened to be mounted next to a specific conversation.
 */
export function ChatExplorer() {
  const navigate = useNavigate();
  const chats = useChatListStore((state) => state.chats);
  const chatIdsWithPendingPrompts = useChatAttention(
    (state) => state.chatIdsWithPendingPrompts,
  );
  const deletingChatId = useChatListStore((state) => state.deletingChatId);
  const [query, setQuery] = useState("");

  const matches = useMemo(
    () => chats.filter((chat) => matchesChatSearch(chat, query)),
    [chats, query],
  );

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3">
      <div className="relative">
          <Search className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            className="pl-8"
            placeholder="Search chats"
            aria-label="Search chats"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
        </div>

        {matches.length === 0 ? (
          // `flex-1` by default, which on a page this tall reads as a huge
          // empty box rather than a note.
          <Empty className="flex-none border">
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <MessageCircleMore />
              </EmptyMedia>
              <EmptyTitle>{chats.length === 0 ? "No chats yet" : "No matches"}</EmptyTitle>
              <EmptyDescription>
                {chats.length === 0
                  ? "Start one and it will show up here."
                  : "No chat title contains that."}
              </EmptyDescription>
            </EmptyHeader>
          </Empty>
        ) : (
          <ul className="flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto">
            {matches.map((chat) => {
              const title = chat.title?.trim() || "New chat";
              return (
                <li key={chat.id}>
                  <button
                    type="button"
                    disabled={deletingChatId !== null}
                    onClick={() =>
                      void navigate({ to: "/c/$chatId", params: { chatId: chat.id } })
                    }
                    className="flex w-full cursor-pointer items-center gap-2 rounded-md px-2 py-2 text-left text-sm transition-colors hover:bg-muted disabled:pointer-events-none disabled:opacity-50"
                  >
                    <MessageCircleMore className="size-4 shrink-0 text-muted-foreground" />
                    <span className="min-w-0 truncate">{title}</span>
                    {chatIdsWithPendingPrompts.has(chat.id) && (
                      <span
                        className="ml-auto shrink-0 text-amber-600 dark:text-amber-400"
                        aria-label={`${title} needs attention`}
                        title="Needs attention"
                      >
                        <CircleAlert aria-hidden="true" className="size-4" />
                      </span>
                    )}
                  </button>
                </li>
              );
            })}
          </ul>
        )}
    </div>
  );
}
