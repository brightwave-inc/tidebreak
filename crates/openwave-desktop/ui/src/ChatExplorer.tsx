import {
  AllCommunityModule,
  ModuleRegistry,
  type ColDef,
} from "ag-grid-community";
import { AgGridReact, type CustomCellRendererProps } from "ag-grid-react";
import { format } from "date-fns";
import { CircleAlert, MessageCircleMore } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";

import type { Chat } from "./api";
import { useChatAttention } from "./ChatAttention";
import { useChatListStore } from "./ChatListStore";
import { SearchInput } from "./components/SearchInput";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "./components/ui/empty";
import { WithTooltip } from "./components/ui/tooltip";
import { useAgGridTheme } from "./sources/useAgGridTheme";

ModuleRegistry.registerModules([AllCommunityModule]);

/** Case-insensitive match on the title, with untitled chats matching "new chat". */
export function matchesChatSearch(chat: Chat, query: string): boolean {
  const trimmed = query.trim().toLowerCase();
  if (!trimmed) return true;
  const title = chat.title?.trim() || "New chat";
  return title.toLowerCase().includes(trimmed);
}

type ChatGridContext = {
  needsAttention: (chatId: string) => boolean;
};

type CellProps = CustomCellRendererProps<Chat>;

function TitleCellRenderer(props: CellProps) {
  const chat = props.data!;
  const context = props.context as ChatGridContext;
  const title = chat.title?.trim() || "New chat";

  return (
    <div className="flex h-full min-w-0 items-center gap-2">
      <span className="truncate text-sm">{title}</span>
      {context.needsAttention(chat.id) && (
        <span
          className="text-warning ml-1 shrink-0"
          aria-label={`${title} needs attention`}
          title="Needs attention"
        >
          <CircleAlert aria-hidden="true" className="size-4" />
        </span>
      )}
    </div>
  );
}

function CreatedCellRenderer(props: CellProps) {
  const createdAt = props.data!.created_at;
  const date = new Date(createdAt);
  return (
    <WithTooltip label={format(date, "MMM dd, yyyy HH:mm")}>
      <time dateTime={createdAt} className="text-sm text-foreground">
        {format(date, "MMM dd")}
      </time>
    </WithTooltip>
  );
}

/**
 * Every conversation, searchable.
 *
 * Finding a chat is something you do between conversations, not inside one, so
 * the table has its own route beside home rather than a place in a
 * conversation's rail.
 */
export function ChatExplorer() {
  const navigate = useNavigate();
  const chats = useChatListStore((state) => state.chats);
  const chatIdsWithPendingPrompts = useChatAttention(
    (state) => state.chatIdsWithPendingPrompts,
  );
  const [query, setQuery] = useState("");
  const gridTheme = useAgGridTheme();
  const gridRef = useRef<AgGridReact<Chat>>(null);

  const matches = useMemo(
    () => chats.filter((chat) => matchesChatSearch(chat, query)),
    [chats, query],
  );

  const gridContext = useMemo<ChatGridContext>(
    () => ({ needsAttention: (chatId) => chatIdsWithPendingPrompts.has(chatId) }),
    [chatIdsWithPendingPrompts],
  );

  // Cell renderers read their state off `context`, which the grid snapshots
  // rather than re-reading, so a changed attention set needs the cells redrawn.
  useEffect(() => {
    gridRef.current?.api?.refreshCells({ force: true });
  }, [gridContext]);

  const columnDefs = useMemo<ColDef<Chat>[]>(
    () => [
      {
        headerName: "Title",
        field: "title",
        flex: 1,
        minWidth: 240,
        cellRenderer: TitleCellRenderer,
        sortable: true,
        comparator: (left: string | null, right: string | null) =>
          (left?.trim() || "New chat").localeCompare(right?.trim() || "New chat", undefined, {
            sensitivity: "base",
          }),
      },
      {
        headerName: "Created",
        field: "created_at",
        width: 130,
        cellRenderer: CreatedCellRenderer,
        sortable: true,
        comparator: (left: string, right: string) => Date.parse(left) - Date.parse(right),
      },
    ],
    [],
  );

  const defaultColDef = useMemo<ColDef>(
    () => ({ resizable: true, suppressMovable: true }),
    [],
  );

  return (
    <div className="flex h-full min-h-0 flex-col gap-4 px-6 pt-6 pb-6">
      <div className="flex shrink-0 flex-wrap items-center justify-between gap-3">
        <h1 className="text-lg font-medium text-foreground">All chats</h1>
        <SearchInput
          placeholder="Search chats"
          aria-label="Search chats"
          value={query}
          onValueChange={setQuery}
          className="max-w-md min-w-64 flex-1"
        />
      </div>

      <div className="relative min-h-0 flex-1">
        {matches.length === 0 ? (
          <Empty className="h-full border">
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
          <div className="size-full">
            <AgGridReact<Chat>
              ref={gridRef}
              theme={gridTheme}
              context={gridContext}
              rowData={matches}
              columnDefs={columnDefs}
              defaultColDef={defaultColDef}
              suppressMovableColumns
              suppressCellFocus
              suppressRowClickSelection
              getRowId={(params) => params.data.id}
              domLayout="normal"
              rowClass="cursor-pointer"
              onRowClicked={(event) => {
                if (!event.data) return;
                void navigate({
                  to: "/c/$chatId",
                  params: { chatId: event.data.id },
                });
              }}
            />
          </div>
        )}
      </div>
    </div>
  );
}
