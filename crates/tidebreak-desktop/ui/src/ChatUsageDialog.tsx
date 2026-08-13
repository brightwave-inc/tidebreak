import { useEffect, useState, type ReactNode } from "react";
import { toast } from "sonner";

import type { ApiClient, Chat } from "./api";
import { useApp } from "./AppContext";
import { chatUsageTotals } from "./ChatUsage";
import { useChatSessionStore } from "./ChatSessionStore";
import {
  contextTokens,
  contextUsageLevel,
  contextUsagePercent,
  formatTokenCount,
} from "./ContextUsage";
import type { ChatTerminalTurnSnapshot, RendererTurnUsage } from "./generated/wire";
import { modelForChat } from "./ModelSelection";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
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
  const { models, defaultModelKey } = useApp();
  const lastTurnUsage = useChatSessionStore((session) => session.lastTurnUsage);
  const [turns, setTurns] = useState<ChatTerminalTurnSnapshot[] | null>(null);
  const model = modelForChat(models, chat.model, defaultModelKey);
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
      <DialogContent className="max-w-sm gap-5 p-5 sm:rounded-xl">
        <DialogHeader className="gap-1 pr-6">
          <DialogTitle className="text-base">Context and usage</DialogTitle>
          <DialogDescription className="text-xs leading-relaxed">
            {model
              ? `Token counts on ${model.display_name}.`
              : "Token counts for this conversation."}
          </DialogDescription>
        </DialogHeader>

        <ContextWindowPanel
          lastTurnUsage={lastTurnUsage}
          contextWindow={contextWindow}
          percent={percent}
        />

        <ChatTotalsPanel turns={turns} totals={totals} />
      </DialogContent>
    </Dialog>
  );
}

function ContextWindowPanel({
  lastTurnUsage,
  contextWindow,
  percent,
}: {
  lastTurnUsage: RendererTurnUsage | null;
  contextWindow: number | undefined;
  percent: number | null;
}) {
  if (lastTurnUsage === null) {
    return (
      <section className="rounded-lg border bg-muted/30 px-3.5 py-4">
        <SectionLabel>Last turn</SectionLabel>
        <p className="mt-2 text-sm text-muted-foreground">
          No turn has finished in this chat yet.
        </p>
      </section>
    );
  }

  const used = contextTokens(lastTurnUsage);
  const metered = percent !== null && contextWindow !== undefined;
  const level = metered ? contextUsageLevel(percent) : "normal";

  return (
    <section className="overflow-hidden rounded-lg border">
      <div
        className={cn(
          "px-3.5 pt-3.5 pb-3",
          level === "critical" && "bg-destructive/10",
          level === "warning" && "bg-warning-background/60",
          level === "normal" && "bg-muted/25",
        )}
      >
        <div className="flex items-start justify-between gap-3">
          <SectionLabel>Last turn</SectionLabel>
          {metered && level !== "normal" && (
            <span
              className={cn(
                "rounded-full px-2 py-0.5 text-2xs font-semibold tracking-wide uppercase",
                level === "critical" &&
                  "bg-destructive/15 text-destructive",
                level === "warning" &&
                  "bg-warning-background text-warning-foreground",
              )}
            >
              {level === "critical" ? "Near full" : "Getting full"}
            </span>
          )}
        </div>

        {metered ? (
          <div className="mt-3 flex items-end gap-3">
            <p
              className={cn(
                "font-mono text-4xl leading-none font-semibold tracking-tight tabular-nums",
                level === "critical" && "text-destructive",
                level === "warning" && "text-warning-foreground",
              )}
              aria-label={`${percent}% of context window used`}
            >
              {percent}
              <span className="ml-0.5 text-xl font-medium opacity-55">%</span>
            </p>
            <div className="min-w-0 flex-1 pb-0.5">
              <p className="truncate font-mono text-xs tabular-nums text-muted-foreground">
                {used.toLocaleString()}
                <span className="mx-1 opacity-40">/</span>
                {contextWindow.toLocaleString()}
              </p>
              <p className="mt-0.5 text-2xs text-muted-foreground">
                of {formatTokenCount(contextWindow)} context
              </p>
            </div>
          </div>
        ) : (
          <div className="mt-3">
            <p className="font-mono text-4xl leading-none font-semibold tracking-tight tabular-nums">
              {formatTokenCount(used)}
            </p>
            <p className="mt-1.5 text-xs text-muted-foreground">
              {used.toLocaleString()} tokens · no published context window
            </p>
          </div>
        )}

        {metered && (
          <WindowMeter percent={percent} level={level} className="mt-3.5" />
        )}
      </div>

      <div className="border-t bg-background px-3.5 py-3">
        <TokenComposition usage={lastTurnUsage} />
      </div>
    </section>
  );
}

function ChatTotalsPanel({
  turns,
  totals,
}: {
  turns: ChatTerminalTurnSnapshot[] | null;
  totals: ReturnType<typeof chatUsageTotals>;
}) {
  return (
    <section className="space-y-2.5">
      <div className="flex items-baseline justify-between gap-2">
        <SectionLabel>This chat</SectionLabel>
        {turns !== null && totals.turns > 0 && (
          <span className="text-2xs text-muted-foreground tabular-nums">
            {totals.turns} {totals.turns === 1 ? "turn" : "turns"}
          </span>
        )}
      </div>

      {turns === null ? (
        <p className="text-sm text-muted-foreground">Reading finished turns…</p>
      ) : totals.turns === 0 ? (
        <p className="text-sm text-muted-foreground">
          Nothing spent yet — no turn has finished.
        </p>
      ) : (
        <UsageBreakdown
          usage={totals}
          totalLabel="Spent so far"
          emphasizeTotal
        />
      )}
    </section>
  );
}

function SectionLabel({ children }: { children: ReactNode }) {
  return (
    <h3 className="text-2xs font-semibold tracking-[0.1em] text-muted-foreground uppercase">
      {children}
    </h3>
  );
}

/**
 * Filled track for share of the model's context window. Colour follows the
 * same warning ladder as the header chip so the two surfaces agree.
 */
function WindowMeter({
  percent,
  level,
  className,
}: {
  percent: number;
  level: ReturnType<typeof contextUsageLevel>;
  className?: string;
}) {
  return (
    <div
      className={cn(
        "h-1.5 w-full overflow-hidden rounded-full bg-foreground/10",
        className,
      )}
      role="meter"
      aria-valuenow={percent}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-label="Share of the context window used"
    >
      <div
        className={cn(
          "h-full rounded-full transition-[width] duration-300 ease-out",
          level === "critical" && "bg-destructive",
          level === "warning" && "bg-warning",
          level === "normal" && "bg-foreground/75",
        )}
        style={{ width: `${percent}%` }}
      />
    </div>
  );
}

type TokenKind = "input" | "output" | "cache_read" | "cache_write";

const TOKEN_KINDS: readonly {
  key: TokenKind;
  label: string;
  short: string;
  barClass: string;
  swatchClass: string;
}[] = [
  {
    key: "input",
    label: "Input",
    short: "In",
    barClass: "bg-foreground/80",
    swatchClass: "bg-foreground/80",
  },
  {
    key: "output",
    label: "Output",
    short: "Out",
    barClass: "bg-info",
    swatchClass: "bg-info",
  },
  {
    key: "cache_read",
    label: "Cache read",
    short: "Cache",
    barClass: "bg-success",
    swatchClass: "bg-success",
  },
  {
    key: "cache_write",
    label: "Cache write",
    short: "Write",
    barClass: "bg-muted-foreground/55",
    swatchClass: "bg-muted-foreground/55",
  },
];

function tokensFor(usage: RendererTurnUsage, key: TokenKind): number {
  switch (key) {
    case "input":
      return usage.input_tokens;
    case "output":
      return usage.output_tokens;
    case "cache_read":
      return usage.cache_read_input_tokens;
    case "cache_write":
      return usage.cache_creation_input_tokens;
  }
}

/**
 * Stacked bar of where the tokens went, plus a compact legend. The signature
 * of this panel: one glance at the strip beats four equal-weight rows.
 */
function TokenComposition({ usage }: { usage: RendererTurnUsage }) {
  const parts = TOKEN_KINDS.map((kind) => ({
    ...kind,
    tokens: tokensFor(usage, kind.key),
  }));
  const total = parts.reduce((sum, part) => sum + part.tokens, 0);
  const active = parts.filter((part) => part.tokens > 0);

  return (
    <div className="space-y-2.5">
      <div
        className="flex h-2 w-full overflow-hidden rounded-full bg-muted"
        role="img"
        aria-label={
          total === 0
            ? "No tokens on this turn"
            : active
                .map(
                  (part) =>
                    `${part.label} ${part.tokens.toLocaleString()} tokens`,
                )
                .join(", ")
        }
      >
        {total > 0 &&
          active.map((part) => (
            <span
              key={part.key}
              className={cn("h-full min-w-px", part.barClass)}
              style={{ width: `${(part.tokens / total) * 100}%` }}
            />
          ))}
      </div>

      <ul className="grid grid-cols-2 gap-x-3 gap-y-1.5">
        {parts.map((part) => {
          const share = total > 0 ? Math.round((part.tokens / total) * 100) : 0;
          const idle = part.tokens === 0;
          return (
            <li
              key={part.key}
              className={cn(
                "flex min-w-0 items-baseline gap-1.5",
                idle && "opacity-40",
              )}
            >
              <span
                aria-hidden="true"
                className={cn(
                  "mt-1 size-1.5 shrink-0 rounded-full",
                  part.swatchClass,
                )}
              />
              <span className="truncate text-2xs text-muted-foreground">
                {part.short}
              </span>
              <span className="ml-auto font-mono text-xs tabular-nums tracking-tight">
                {formatTokenCount(part.tokens)}
              </span>
              {!idle && total > 0 && (
                <span className="w-7 text-right font-mono text-2xs tabular-nums text-muted-foreground">
                  {share}%
                </span>
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}

function UsageBreakdown({
  usage,
  totalLabel,
  emphasizeTotal = false,
}: {
  usage: RendererTurnUsage;
  totalLabel: string;
  emphasizeTotal?: boolean;
}) {
  const parts = TOKEN_KINDS.map((kind) => ({
    ...kind,
    tokens: tokensFor(usage, kind.key),
  }));
  const total = parts.reduce((sum, part) => sum + part.tokens, 0);
  const max = Math.max(...parts.map((part) => part.tokens), 1);

  return (
    <div className="overflow-hidden rounded-lg border">
      <dl className="divide-y divide-border/70">
        {parts.map((part) => {
          const idle = part.tokens === 0;
          const width = idle ? 0 : Math.max(4, (part.tokens / max) * 100);
          return (
            <div
              key={part.key}
              className={cn(
                "grid grid-cols-[auto_1fr_auto] items-center gap-2.5 px-3 py-2",
                idle && "opacity-45",
              )}
            >
              <dt className="flex w-24 items-center gap-2 text-sm text-muted-foreground">
                <span
                  aria-hidden="true"
                  className={cn("size-1.5 shrink-0 rounded-full", part.swatchClass)}
                />
                {part.label}
              </dt>
              <dd className="min-w-0">
                <div className="h-1 overflow-hidden rounded-full bg-muted">
                  <div
                    className={cn("h-full rounded-full", part.barClass)}
                    style={{ width: `${width}%` }}
                  />
                </div>
              </dd>
              <dd className="w-16 text-right font-mono text-sm tabular-nums tracking-tight">
                {part.tokens.toLocaleString()}
              </dd>
            </div>
          );
        })}
      </dl>
      <div
        className={cn(
          "flex items-baseline justify-between gap-3 border-t px-3 py-2.5",
          emphasizeTotal ? "bg-muted/40" : "bg-muted/20",
        )}
      >
        <span className="text-sm font-medium">{totalLabel}</span>
        <span className="font-mono text-sm font-semibold tabular-nums tracking-tight">
          {total.toLocaleString()}
        </span>
      </div>
    </div>
  );
}
