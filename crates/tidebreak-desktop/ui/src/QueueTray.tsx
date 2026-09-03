import { useCallback, useEffect, useMemo, useState } from "react";
import {
  ArrowUpRight,
  ChevronDown,
  ChevronUp,
  Pause,
  Pencil,
  Play,
  Trash2,
} from "lucide-react";
import { toast } from "sonner";

import type { ApiClient } from "./api";
import { friendlyErrorMessage } from "./lib/utils";
import { useRefreshSignals } from "./RefreshSignals";
import { useVisibilityGatedPoll } from "./useVisibilityGatedPoll";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";

/** Safety net under the turn-boundary and enqueue signals, while a turn is live or rows show. */
const QUEUE_POLL_MS = 15_000;

/** One queued message, in the vocabulary the tray renders. */
export type QueueTrayRow = { id: string; content: string };

/**
 * The queue operations the tray drives. Chat and code sessions expose the
 * same five verbs over different routes; the adapters below map each surface
 * onto this shape so both modes share one tray.
 */
export type QueueTrayApi = {
  list: () => Promise<{ queued: QueueTrayRow[]; paused: boolean }>;
  update: (
    id: string,
    change: { content?: string; position?: number },
  ) => Promise<unknown>;
  remove: (id: string) => Promise<unknown>;
  setPaused: (paused: boolean) => Promise<unknown>;
  sendNow: () => Promise<unknown>;
};

/** The chat queue (`/chats/{id}/queued`), decision 9. */
export function chatQueueApi(client: ApiClient, chatId: string): QueueTrayApi {
  return {
    list: async () => {
      const snapshot = await client.listQueuedTurns(chatId);
      return {
        queued: snapshot.queued.map((row) => ({
          id: row.id,
          content: row.content,
        })),
        paused: snapshot.paused,
      };
    },
    update: (id, change) => client.patchQueuedTurn(chatId, id, change),
    remove: (id) => client.deleteQueuedTurn(chatId, id),
    setPaused: (paused) => client.putQueuePaused(chatId, paused),
    sendNow: () => client.sendQueuedNow(chatId),
  };
}

/** The code-session queue (`/sessions/{id}/queued`), decision 69. */
export function codeQueueApi(
  client: ApiClient,
  sessionId: string,
): QueueTrayApi {
  return {
    list: async () => {
      const snapshot = await client.listCodeQueuedTurns(sessionId);
      return {
        queued: snapshot.queued.map((row) => ({
          id: row.id,
          content: row.message,
        })),
        paused: snapshot.paused,
      };
    },
    update: (id, change) =>
      client.patchCodeQueuedTurn(sessionId, id, {
        ...(change.content !== undefined ? { message: change.content } : {}),
        ...(change.position !== undefined ? { position: change.position } : {}),
      }),
    remove: (id) => client.deleteCodeQueuedTurn(sessionId, id),
    setPaused: (paused) => client.putCodeQueuePaused(sessionId, paused),
    sendNow: () => client.sendCodeQueuedNow(sessionId),
  };
}

/**
 * The durable message queue, rendered directly above the composer.
 *
 * Every row is a queued turn the server owns: reorder, edit, and delete are
 * real API calls, the queue survives restarts, and promotion happens
 * server-side the moment the conversation is free — this component only
 * observes. Hidden entirely while the queue is empty, so the ordinary
 * composer is untouched until the first mid-turn send.
 */
export function QueueTray({
  queue,
  active,
  onStop,
}: {
  queue: QueueTrayApi;
  active: boolean;
  onStop: () => Promise<void>;
}) {
  const [queued, setQueued] = useState<QueueTrayRow[]>([]);
  const [paused, setPaused] = useState(false);
  const [editing, setEditing] = useState<string | null>(null);
  const [editDraft, setEditDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const queuedSignal = useRefreshSignals((state) => state.queuedTurns);

  const refresh = useCallback(async () => {
    try {
      const snapshot = await queue.list();
      setQueued(snapshot.queued);
      setPaused(snapshot.paused);
    } catch {
      // Poll again on the next tick; a queue read is never worth a toast.
    }
  }, [queue]);

  // Read when the queue or the turn ahead of it changes: a mount, the
  // active turn starting or ending, and every `queuedTurns` signal (a send
  // parked a row, a turn boundary let the promoter run). The slow timer
  // below is the net under those, and only while there is anything to watch.
  useEffect(() => {
    void refresh();
  }, [refresh, active]);
  useVisibilityGatedPoll(() => void refresh(), QUEUE_POLL_MS, {
    enabled: active || queued.length > 0,
    revision: queuedSignal,
  });

  async function act(action: () => Promise<unknown>, failure: string) {
    setBusy(true);
    try {
      await action();
      await refresh();
    } catch (error) {
      toast.error(friendlyErrorMessage(error, failure));
    } finally {
      setBusy(false);
    }
  }

  async function sendNow(row: QueueTrayRow) {
    setBusy(true);
    let temporarilyPaused = false;
    try {
      if (!paused) {
        await queue.setPaused(true);
        temporarilyPaused = true;
      }
      await queue.update(row.id, { position: 0 });
      if (active) await onStop();
      await queue.sendNow();
      temporarilyPaused = false;
      await refresh();
    } catch (error) {
      toast.error(
        friendlyErrorMessage(error, "Could not send that message now"),
      );
      if (temporarilyPaused) {
        try {
          await queue.setPaused(false);
        } catch {
          // The original failure is the useful one; polling will show the
          // actual queue state and the header still offers Resume.
        }
      }
      await refresh();
    } finally {
      setBusy(false);
    }
  }

  if (queued.length === 0) return null;

  return (
    <section
      aria-label="Queued messages"
      className="mx-auto mb-2 w-full max-w-3xl overflow-hidden rounded-xl border border-border/60 bg-card/80 shadow-sm"
    >
      <header className="flex h-8 items-center gap-2 border-b border-border/60 pl-3 pr-1.5">
        <span className="flex min-w-0 items-baseline gap-1.5 text-xs">
          <span className="font-medium">Queued</span>
          <span className="tabular-nums text-muted-foreground">
            {queued.length}
          </span>
        </span>
        {paused && (
          <span className="rounded-full border border-warning-border/45 bg-warning-background px-1.5 py-px text-2xs font-medium leading-4 text-warning-foreground">
            Paused
          </span>
        )}
        <div className="ml-auto flex items-center gap-0.5">
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            className="size-6 text-muted-foreground hover:text-foreground"
            aria-label={paused ? "Resume queue" : "Pause queue"}
            disabled={busy}
            onClick={() =>
              void act(
                () => queue.setPaused(!paused),
                "Could not update the queue",
              )
            }
          >
            {paused ? (
              <Play className="size-3" />
            ) : (
              <Pause className="size-3" />
            )}
          </Button>
        </div>
      </header>
      <ul className="divide-y divide-border/60 px-3">
        {queued.map((row, index) => (
          <li key={row.id} className="group flex h-8 items-center gap-2">
            <span className="w-3 shrink-0 text-right text-xs tabular-nums text-muted-foreground">
              {index + 1}
            </span>
            {editing === row.id ? (
              <Textarea
                rows={1}
                autoFocus
                className="min-h-7 flex-1 py-1 text-sm"
                value={editDraft}
                onChange={(event) => setEditDraft(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" && !event.shiftKey) {
                    event.preventDefault();
                    const content = editDraft.trim();
                    setEditing(null);
                    if (content && content !== row.content) {
                      void act(
                        () => queue.update(row.id, { content }),
                        "Could not edit the queued message",
                      );
                    }
                  }
                  if (event.key === "Escape") setEditing(null);
                }}
                onBlur={() => setEditing(null)}
              />
            ) : (
              <span className="flex-1 truncate text-sm" title={row.content}>
                {row.content}
              </span>
            )}
            <Button
              type="button"
              variant="ghost"
              size="xs"
              className="h-6 shrink-0 gap-1 px-2 text-xs text-muted-foreground hover:text-foreground"
              aria-label={`Send queued message ${index + 1} now`}
              disabled={busy}
              onClick={() => void sendNow(row)}
            >
              <ArrowUpRight className="size-3" />
              Send now
            </Button>
            <span className="hidden shrink-0 items-center gap-0.5 group-hover:inline-flex group-focus-within:inline-flex">
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                className="size-6 text-muted-foreground hover:text-foreground"
                aria-label="Move up"
                disabled={busy || index === 0}
                onClick={() =>
                  void act(
                    () => queue.update(row.id, { position: index - 1 }),
                    "Could not reorder the queue",
                  )
                }
              >
                <ChevronUp className="size-3.5" />
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                className="size-6 text-muted-foreground hover:text-foreground"
                aria-label="Move down"
                disabled={busy || index === queued.length - 1}
                onClick={() =>
                  void act(
                    () => queue.update(row.id, { position: index + 1 }),
                    "Could not reorder the queue",
                  )
                }
              >
                <ChevronDown className="size-3.5" />
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                className="size-6 text-muted-foreground hover:text-foreground"
                aria-label="Edit queued message"
                disabled={busy}
                onClick={() => {
                  setEditing(row.id);
                  setEditDraft(row.content);
                }}
              >
                <Pencil className="size-3.5" />
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                className="size-6 text-muted-foreground hover:text-destructive"
                aria-label="Delete queued message"
                disabled={busy}
                onClick={() =>
                  void act(
                    () => queue.remove(row.id),
                    "Could not delete the queued message",
                  )
                }
              >
                <Trash2 className="size-3.5" />
              </Button>
            </span>
          </li>
        ))}
      </ul>
    </section>
  );
}

/** Memoize a stable adapter so the tray's poll effect does not rearm per render. */
export function useChatQueueApi(client: ApiClient, chatId: string) {
  return useMemo(() => chatQueueApi(client, chatId), [client, chatId]);
}

/** Memoize a stable adapter so the tray's poll effect does not rearm per render. */
export function useCodeQueueApi(client: ApiClient, sessionId: string) {
  return useMemo(() => codeQueueApi(client, sessionId), [client, sessionId]);
}
