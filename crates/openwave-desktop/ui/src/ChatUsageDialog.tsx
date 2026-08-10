import { useEffect, useState } from "react";
import { toast } from "sonner";

import type { ApiClient, Chat } from "./api";
import { useApp } from "./AppContext";
import { chatUsageTotals } from "./ChatUsage";
import { useChatSessionStore } from "./ChatSessionStore";
import {
  contextTokens,
  contextUsageLevel,
  contextUsagePercent,
} from "./ContextUsage";
import type { ChatTerminalTurnSnapshot } from "./generated/wire";
import { modelForSelection } from "./ModelSelection";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Progress } from "@/components/ui/progress";
import { cn } from "@/lib/utils";
import { friendlyErrorMessage } from "@/lib/utils";

/**
 * What `/usage` shows: how full the window is, and what the chat has spent.
 *
 * Everything here is read, not sent — no turn is started to answer the
 * question. The window reading comes from the session the composer is already
 * metering; the running totals are folded from the durable turn rows, which is
 * the only place a chat's earlier turns are still counted after a reload.
 *
 * Token counts only. A price would need the rate the reader's own key is billed
 * at, which the app does not know, and a wrong number about money is worse than
 * no number at all.
 */
export function ChatUsageDialog({
  client,
  chat,
  open,
  onOpenChange,
}: {
  client: Pick<ApiClient, "listChatMessages">;
  chat: Chat;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const { models } = useApp();
  const lastTurnUsage = useChatSessionStore((session) => session.lastTurnUsage);
  const [turns, setTurns] = useState<ChatTerminalTurnSnapshot[] | null>(null);
  const model = modelForSelection(models, chat.model);
  const contextWindow = model?.context_window ?? undefined;
  const percent = lastTurnUsage
    ? contextUsagePercent(lastTurnUsage, contextWindow)
    : null;
  const totals = chatUsageTotals(turns ?? undefined);

  // Re-read on every open: turns finish while the dialog is closed, and a
  // stale total is indistinguishable from a chat that has done nothing since.
  useEffect(() => {
    if (!open) return;
    let current = true;
    void (async () => {
      try {
        const transcript = await client.listChatMessages(chat.id);
        if (current) setTurns(transcript.terminal_turns);
      } catch (caught) {
        if (!current) return;
        toast.error(
          friendlyErrorMessage(caught, "Could not read this chat's usage."),
        );
      }
    })();
    return () => {
      current = false;
    };
  }, [client, chat.id, open]);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md gap-6">
        <DialogHeader>
          <DialogTitle>Context and usage</DialogTitle>
          <DialogDescription>
            Token counts for this conversation{model ? ` on ${model.display_name}` : ""}.
          </DialogDescription>
        </DialogHeader>
        <section className="space-y-2">
          <h3 className="text-2xs font-semibold tracking-[0.08em] text-muted-foreground uppercase">
            Context window
          </h3>
          {lastTurnUsage === null ? (
            <p className="text-sm text-muted-foreground">
              No turn has finished in this chat yet.
            </p>
          ) : percent === null || contextWindow === undefined ? (
            <p className="text-sm text-muted-foreground">
              {contextTokens(lastTurnUsage).toLocaleString()} tokens on the last
              turn. This model does not publish a context window, so there is
              nothing honest to meter it against.
            </p>
          ) : (
            <>
              <Progress
                value={percent}
                className={cn(
                  contextUsageLevel(percent) === "critical" && "bg-destructive/20",
                )}
                aria-label="Share of the context window used"
              />
              <p className="text-sm tabular-nums">
                {contextTokens(lastTurnUsage).toLocaleString()} of{" "}
                {contextWindow.toLocaleString()} tokens ({percent}%)
              </p>
            </>
          )}
          {lastTurnUsage && (
            <UsageRows
              label="Last turn"
              rows={[
                ["Input", lastTurnUsage.input_tokens],
                ["Output", lastTurnUsage.output_tokens],
                ["Cache read", lastTurnUsage.cache_read_input_tokens],
                ["Cache write", lastTurnUsage.cache_creation_input_tokens],
              ]}
            />
          )}
        </section>
        <section className="space-y-2">
          <h3 className="text-2xs font-semibold tracking-[0.08em] text-muted-foreground uppercase">
            This chat
          </h3>
          {turns === null ? (
            <p className="text-sm text-muted-foreground">Reading finished turns…</p>
          ) : totals.turns === 0 ? (
            <p className="text-sm text-muted-foreground">
              Nothing spent yet — no turn has finished.
            </p>
          ) : (
            <UsageRows
              label={`${totals.turns} ${totals.turns === 1 ? "turn" : "turns"}`}
              rows={[
                ["Input", totals.input_tokens],
                ["Output", totals.output_tokens],
                ["Cache read", totals.cache_read_input_tokens],
                ["Cache write", totals.cache_creation_input_tokens],
                [
                  "Total",
                  totals.input_tokens +
                    totals.output_tokens +
                    totals.cache_read_input_tokens +
                    totals.cache_creation_input_tokens,
                ],
              ]}
            />
          )}
        </section>
      </DialogContent>
    </Dialog>
  );
}

function UsageRows({
  label,
  rows,
}: {
  label: string;
  rows: readonly (readonly [string, number])[];
}) {
  return (
    <dl className="grid grid-cols-[1fr_auto] gap-x-6 gap-y-1" aria-label={label}>
      {rows.map(([name, tokens]) => (
        <div key={name} className="col-span-2 grid grid-cols-subgrid">
          <dt className="text-sm text-muted-foreground">{name}</dt>
          <dd className="text-sm tabular-nums">{tokens.toLocaleString()}</dd>
        </div>
      ))}
    </dl>
  );
}
