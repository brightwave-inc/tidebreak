import { useCallback, useEffect, useRef, useState } from "react";
import { ChevronDown, ChevronUp, Pause, Pencil, Play, Trash2 } from "lucide-react";
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
}: {
  client: ApiClient;
  chatId: string;
  active: boolean;
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

  if (queued.length === 0) return null;

  return (
    <section
      aria-label="Queued messages"
      className="mx-auto mb-2 w-full max-w-3xl rounded-xl border border-border bg-card shadow-sm"
    >
      <header className="flex items-center gap-2 border-b border-border px-3 py-1.5">
        <span className="text-xs font-semibold">
          Queued <span className="font-normal text-muted-foreground">{queued.length}</span>
          {paused && (
            <span className="ml-2 rounded-full border border-warning-border/45 bg-warning-background px-2 py-px text-[11px] font-medium text-warning-foreground">
              Paused
            </span>
          )}
        </span>
        <div className="ml-auto">
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="h-6 gap-1 px-2 text-xs text-muted-foreground"
            disabled={busy}
            onClick={() =>
              void act(
                () => client.putQueuePaused(chatId, !paused),
                "Could not update the queue",
              )
            }
          >
            {paused ? <Play className="size-3" /> : <Pause className="size-3" />}
            {paused ? "Resume" : "Pause"}
          </Button>
        </div>
      </header>
      <ul className="flex flex-col gap-px p-1.5">
        {queued.map((row, index) => (
          <li
            key={row.id}
            className="group flex items-center gap-1.5 rounded-md px-1.5 py-1 hover:bg-accent"
          >
            <span className="w-4 shrink-0 text-right text-[11px] tabular-nums text-muted-foreground">
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
                        () => client.patchQueuedTurn(chatId, row.id, { content }),
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
            <span className="hidden shrink-0 items-center group-hover:inline-flex">
              <Button
                type="button"
                variant="ghost"
                size="icon-8"
                className="size-6"
                aria-label="Move up"
                disabled={busy || index === 0}
                onClick={() =>
                  void act(
                    () => client.patchQueuedTurn(chatId, row.id, { position: index - 1 }),
                    "Could not reorder the queue",
                  )
                }
              >
                <ChevronUp className="size-3.5" />
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="icon-8"
                className="size-6"
                aria-label="Move down"
                disabled={busy || index === queued.length - 1}
                onClick={() =>
                  void act(
                    () => client.patchQueuedTurn(chatId, row.id, { position: index + 1 }),
                    "Could not reorder the queue",
                  )
                }
              >
                <ChevronDown className="size-3.5" />
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="icon-8"
                className="size-6"
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
                size="icon-8"
                className="size-6 hover:text-destructive"
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
