import { useEffect, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { formatDistanceToNowStrict } from "date-fns";
import { Archive, ArchiveRestore, Trash2 } from "lucide-react";

import type { Chat } from "./api";
import { useApp } from "./AppContext";
import { lastActivityAt, predatesListState } from "./chatListGroups";
import { useChatListStore } from "./ChatListStore";
import { useProjectListStore } from "./ProjectListStore";
import { Loader } from "@/components/motion/loader";
import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { Skeleton } from "@/components/ui/skeleton";
import { friendlyErrorMessage } from "@/lib/utils";
import { paneHeaderDragRegion } from "./WindowDragStrip";
import { Notice, NoticeRetryButton } from "@/components/ui/notice";

function archivedAgo(chat: Chat): string {
  const at = Date.parse(chat.archived_at ?? lastActivityAt(chat));
  if (Number.isNaN(at)) return "";
  return `Archived ${formatDistanceToNowStrict(at, { addSuffix: true })}`;
}

/**
 * Archived work: conversations out of the list of work, not deleted.
 *
 * An Index surface. Each row opens its conversation, brings it back into the
 * list, or deletes it through the same confirmation the rail uses. The page
 * reads the archive from the server each time it opens, so a conversation
 * archived from the CLI or the phone shows up here too.
 */
export function WorkArchivePage() {
  const { client } = useApp();
  const archived = useChatListStore((state) => state.archivedChats);
  const loaded = useChatListStore((state) => state.archivedLoaded);
  const [error, setError] = useState<string | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    let cancelled = false;
    // A retry clears the failure so the loading rows show while it runs.
    if (attempt > 0) setError(null);
    client.listChats({ archived: true }).then(
      (chats) => {
        if (cancelled) return;
        setError(null);
        // A server older than the archive does not know the question and
        // answers with every conversation. None of those is archived, so none
        // of them belongs here.
        if (predatesListState(chats)) {
          setUnavailable(true);
          return;
        }
        useChatListStore.getState().setArchivedChats(chats);
      },
      (err) => {
        if (!cancelled) {
          setError(friendlyErrorMessage(err, "Try again in a moment."));
        }
      },
    );
    return () => {
      cancelled = true;
    };
  }, [client, attempt]);

  return (
    <div className="content-container min-h-0 w-full min-w-0 flex-1 overflow-hidden">
      <div className="flex size-full min-h-0 flex-col bg-background">
        <header
          className="shrink-0 border-b border-border-subtle px-5 py-4"
          {...paneHeaderDragRegion()}
        >
          <div className="flex flex-wrap items-baseline gap-x-2">
            <h1 className="text-xl font-semibold tracking-tight">Archive</h1>
            {/* The empty state says "none" already; a zero count beside the
                title would say it twice. */}
            {loaded && archived.length > 0 && (
              <span className="whitespace-nowrap text-xs text-muted-foreground">
                {archived.length} conversation{archived.length === 1 ? "" : "s"}
              </span>
            )}
          </div>
          <p className="mt-0.5 text-sm text-muted-foreground">
            Archived work keeps its messages and outputs. Open it, bring it back
            to your list, or delete it.
          </p>
        </header>
        <div className="min-h-0 flex-1 overflow-auto">
          <ArchiveBody
            archived={archived}
            loaded={loaded}
            error={error}
            unavailable={unavailable}
            onRetry={() => setAttempt((count) => count + 1)}
          />
        </div>
      </div>
    </div>
  );
}

function ArchiveBody({
  archived,
  loaded,
  error,
  unavailable,
  onRetry,
}: {
  archived: Chat[];
  loaded: boolean;
  error: string | null;
  /** The server is older than the archive. */
  unavailable: boolean;
  onRetry: () => void;
}) {
  if (unavailable) {
    return (
      <Empty className="min-h-80">
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <Archive />
          </EmptyMedia>
          <EmptyTitle>The archive needs a newer server</EmptyTitle>
          <EmptyDescription>
            This window is attached to a server that predates the archive.
            Update that server to archive work and bring it back.
          </EmptyDescription>
        </EmptyHeader>
      </Empty>
    );
  }
  if (error && !loaded) {
    return (
      <Notice
        tone="critical"
        title="Could not load the archive"
        className="m-5 w-auto"
        action={<NoticeRetryButton onClick={onRetry} />}
      >
        {error}
      </Notice>
    );
  }
  if (!loaded) {
    return (
      <div className="flex flex-col gap-3 px-5 py-4" aria-label="Loading">
        {[0, 1, 2].map((row) => (
          <Skeleton key={row} className="h-10 w-full" />
        ))}
      </div>
    );
  }
  if (archived.length === 0) {
    return (
      <Empty className="min-h-80">
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <Archive />
          </EmptyMedia>
          <EmptyTitle>No archived work</EmptyTitle>
          <EmptyDescription>
            Archive work from its menu in the rail. It keeps everything and
            stays out of your list until you bring it back.
          </EmptyDescription>
        </EmptyHeader>
      </Empty>
    );
  }
  return (
    <ul aria-label="Archived work" className="flex flex-col">
      {archived.map((chat) => (
        <ArchivedRow key={chat.id} chat={chat} />
      ))}
    </ul>
  );
}

function ArchivedRow({ chat }: { chat: Chat }) {
  const navigate = useNavigate();
  const { unarchiveChat, deleteChat } = useApp();
  const projects = useProjectListStore((state) => state.projects);
  const deletingChatId = useChatListStore((state) => state.deletingChatId);
  const title = chat.title?.trim() || "New work";
  const project = chat.project_id
    ? projects.find((candidate) => candidate.id === chat.project_id)
    : undefined;
  const detail = [project?.title?.trim(), archivedAgo(chat)]
    .filter(Boolean)
    .join(" · ");

  return (
    <li className="flex flex-wrap items-center gap-x-3 gap-y-1.5 border-b border-border-subtle px-5 py-2.5">
      <button
        type="button"
        className="min-w-0 flex-1 basis-40 cursor-pointer rounded-sm text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        onClick={() =>
          void navigate({ to: "/c/$chatId", params: { chatId: chat.id } })
        }
      >
        <span className="flex min-w-0 items-center gap-2">
          <span className="truncate text-sm font-medium" title={title}>
            {title}
          </span>
          {chat.running && (
            <span className="shrink-0" title="Working">
              <Loader
                variant="comet"
                size={12}
                className="text-live"
                decorative
              />
              <span className="sr-only">, working</span>
            </span>
          )}
        </span>
        {detail && (
          <span className="mt-0.5 block truncate text-xs text-muted-foreground">
            {detail}
          </span>
        )}
      </button>
      <div className="flex shrink-0 items-center gap-1.5">
        <Button
          type="button"
          size="xs"
          variant="outline"
          onClick={() => unarchiveChat(chat)}
        >
          <ArchiveRestore />
          Unarchive
        </Button>
        <Button
          type="button"
          size="xs"
          variant="ghost-destructive"
          aria-label={`Delete ${title}`}
          disabled={deletingChatId !== null}
          onClick={() => deleteChat(chat)}
        >
          <Trash2 />
        </Button>
      </div>
    </li>
  );
}
