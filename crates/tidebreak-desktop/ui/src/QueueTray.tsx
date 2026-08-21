import { useCallback, useEffect, useRef, useState } from "react";
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

import type { ApiClient, QueuedTurn } from "./api";
import { friendlyErrorMessage } from "./lib/utils";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";

/**
 * The durable message queue, rendered directly above the composer.
 *
 * Every row is a `queued_turn` the server owns: reorder, edit, and delete are
 * real API calls, the queue survives restarts, and promotion happens
 * server-side the moment the chat is free — this component only observes.
 * Hidden entirely while the queue is empty, so the ordinary composer is
 * untouched until the first mid-turn send.
 */
export function QueueTray({
  client,
  chatId,
  active,
  onStop,
}: {
  client: ApiClient;
  chatId: string;
  active: boolean;
  onStop: () => Promise<void>;
}) {
  const [queued, setQueued] = useState<QueuedTurn[]>([]);
  const [paused, setPaused] = useState(false);
  const [editing, setEditing] = useState<string | null>(null);
  const [editDraft, setEditDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const timerRef = useRef<number | null>(null);

  const refresh = useCallback(async () => {
    if (!client) return;
    try {
      const snapshot = await client.listQueuedTurns(chatId);
      setQueued(snapshot.queued);
      setPaused(snapshot.paused);
    } catch {
      // Poll again on the next tick; a queue read is never worth a toast.
    }
  }, [client, chatId]);

  useEffect(() => {
    void refresh();
    // Poll while a turn is live (promotion imminent) or rows remain visible.
    if (!active && queued.length === 0) return;
    timerRef.current = window.setInterval(() => void refresh(), 1500);
    return () => {
      if (timerRef.current !== null) window.clearInterval(timerRef.current);
    };
  }, [refresh, active, queued.length > 0]);

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

  async function sendNow(row: QueuedTurn) {
    setBusy(true);
    let temporarilyPaused = false;
    try {
      if (!paused) {
        await client.putQueuePaused(chatId, true);
        temporarilyPaused = true;
      }
      await client.patchQueuedTurn(chatId, row.id, { position: 0 });
      if (active) await onStop();
      await client.sendQueuedNow(chatId);
      temporarilyPaused = false;
      await refresh();
    } catch (error) {
      toast.error(
        friendlyErrorMessage(error, "Could not send that message now"),
      );
      if (temporarilyPaused) {
        try {
          await client.putQueuePaused(chatId, false);
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
          <span className="rounded-full border border-warning-border/45 bg-warning-background px-1.5 py-px text-[10px] font-medium leading-4 text-warning-foreground">
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
                () => client.putQueuePaused(chatId, !paused),
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
            <span className="w-3 shrink-0 text-right text-[11px] tabular-nums text-muted-foreground">
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
                        () =>
                          client.patchQueuedTurn(chatId, row.id, { content }),
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
                    () =>
                      client.patchQueuedTurn(chatId, row.id, {
                        position: index - 1,
                      }),
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
                    () =>
                      client.patchQueuedTurn(chatId, row.id, {
                        position: index + 1,
                      }),
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
                    () => client.deleteQueuedTurn(chatId, row.id),
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
