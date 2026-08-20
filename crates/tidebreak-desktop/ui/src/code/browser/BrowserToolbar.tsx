import { useEffect, useId, useRef, type FormEvent } from "react";
import {
  AlertTriangle,
  ArrowLeft,
  ArrowRight,
  ExternalLink,
  History,
  Laptop,
  LoaderCircle,
  LockKeyhole,
  RefreshCw,
  Search,
  ShieldAlert,
  X,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { FOCUS_RING_TIGHT, HOVER_TINT } from "../interactive";
import {
  browserSecurity,
  MAX_BROWSER_URL_CHARS,
} from "./browserNavigation";
import type { BrowserSession } from "./browserSession";

export function BrowserToolbar({
  session,
  address,
  addressError,
  canGoBack,
  canGoForward,
  onAddressChange,
  onNavigate,
  onBack,
  onForward,
  onReload,
  onStop,
  onSelectHistory,
  onOpenExternal,
  onOverlayOpenChange,
}: {
  session: BrowserSession;
  address: string;
  addressError: string | null;
  canGoBack: boolean;
  canGoForward: boolean;
  onAddressChange: (value: string) => void;
  onNavigate: () => void;
  onBack: () => void;
  onForward: () => void;
  onReload: () => void;
  onStop: () => void;
  onSelectHistory: (index: number) => void;
  onOpenExternal: () => void;
  onOverlayOpenChange: (open: boolean) => void;
}) {
  const inputRef = useRef<HTMLInputElement | null>(null);
  const addressErrorId = useId();
  const securityId = useId();
  const security = session.url ? browserSecurity(session.url) : null;

  useEffect(() => {
    if (!session.url) inputRef.current?.focus();
  }, [session.url]);

  function submit(event: FormEvent) {
    event.preventDefault();
    onNavigate();
  }

  return (
    <div className="shrink-0 border-b bg-background px-2 py-1.5">
      <div className="flex min-w-0 items-center gap-1">
        <WithTooltip label="Back">
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            disabled={!canGoBack}
            onClick={onBack}
          >
            <ArrowLeft />
            <span className="sr-only">Back</span>
          </Button>
        </WithTooltip>
        <WithTooltip label="Forward">
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            disabled={!canGoForward}
            onClick={onForward}
          >
            <ArrowRight />
            <span className="sr-only">Forward</span>
          </Button>
        </WithTooltip>
        <WithTooltip label={session.loadState === "loading" ? "Stop" : "Reload"}>
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            disabled={!session.url}
            onClick={session.loadState === "loading" ? onStop : onReload}
          >
            {session.loadState === "loading" ? (
              <X />
            ) : (
              <RefreshCw />
            )}
            <span className="sr-only">
              {session.loadState === "loading" ? "Stop" : "Reload"}
            </span>
          </Button>
        </WithTooltip>

        <form className="min-w-0 flex-1" onSubmit={submit}>
          <label
            className={cn(
              "flex h-control-sm min-w-0 items-center gap-2 rounded-md border bg-muted/35 px-2 text-xs",
              "focus-within:border-ring focus-within:ring-3 focus-within:ring-ring/25",
              addressError && "border-critical/50 ring-2 ring-critical/10",
            )}
          >
            <SecurityIcon id={securityId} security={security?.kind} />
            <input
              ref={inputRef}
              value={address}
              type="text"
              inputMode="url"
              autoCapitalize="none"
              autoCorrect="off"
              spellCheck={false}
              maxLength={MAX_BROWSER_URL_CHARS}
              placeholder="Address or search"
              aria-label="Address or search"
              aria-invalid={Boolean(addressError)}
              aria-describedby={[
                security ? securityId : null,
                addressError ? addressErrorId : null,
              ]
                .filter(Boolean)
                .join(" ") || undefined}
              className="min-w-0 flex-1 bg-transparent text-foreground outline-none placeholder:text-muted-foreground"
              onChange={(event) => onAddressChange(event.target.value)}
              onFocus={(event) => event.currentTarget.select()}
            />
            {session.loadState === "loading" ? (
              <LoaderCircle className="size-3.5 shrink-0 motion-safe:animate-spin text-muted-foreground" />
            ) : (
              <Search className="size-3.5 shrink-0 text-muted-foreground" />
            )}
          </label>
        </form>

        <DropdownMenu onOpenChange={onOverlayOpenChange}>
          <WithTooltip label="History">
            <DropdownMenuTrigger asChild>
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                disabled={session.history.length === 0}
              >
                <History />
                <span className="sr-only">History</span>
              </Button>
            </DropdownMenuTrigger>
          </WithTooltip>
          <DropdownMenuContent
            align="end"
            className="max-h-[min(24rem,var(--radix-dropdown-menu-content-available-height))] w-80 overflow-y-auto"
          >
            {[...session.history].reverse().map((entry, reverseIndex) => {
              const index = session.history.length - reverseIndex - 1;
              const current = index === session.historyIndex;
              return (
                <DropdownMenuItem
                  key={`${entry.url}-${index}`}
                  className="items-start gap-2"
                  aria-current={current ? "page" : undefined}
                  onSelect={() => onSelectHistory(index)}
                >
                  <span
                    aria-hidden
                    className={cn(
                      "mt-2 size-1.5 shrink-0 rounded-full",
                      current ? "bg-primary" : "bg-transparent",
                    )}
                  />
                  <span className="min-w-0">
                    <span className="block truncate text-xs font-medium">
                      {entry.title || entry.url}
                    </span>
                    {entry.title && (
                      <span className="block truncate text-[11px] text-muted-foreground">
                        {entry.url}
                      </span>
                    )}
                  </span>
                </DropdownMenuItem>
              );
            })}
          </DropdownMenuContent>
        </DropdownMenu>

        <WithTooltip label="Open externally">
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            disabled={!session.url}
            onClick={onOpenExternal}
          >
            <ExternalLink />
            <span className="sr-only">Open externally</span>
          </Button>
        </WithTooltip>
      </div>
      {addressError && (
        <div className="px-1 pt-1">
          <p
            id={addressErrorId}
            role="alert"
            className="truncate text-[11px] text-critical"
          >
            {addressError}
          </p>
        </div>
      )}
    </div>
  );
}

function SecurityIcon({
  id,
  security,
}: {
  id: string;
  security: "secure" | "local" | "insecure" | undefined;
}) {
  const className = "size-3.5 shrink-0";
  if (security === "secure") {
    return (
      <>
        <LockKeyhole className={cn(className, "text-success")} />
        <span id={id} className="sr-only">Secure</span>
      </>
    );
  }
  if (security === "local") {
    return (
      <>
        <Laptop className={cn(className, "text-info")} />
        <span id={id} className="sr-only">Local</span>
      </>
    );
  }
  if (security === "insecure") {
    return (
      <>
        <ShieldAlert className={cn(className, "text-warning")} />
        <span id={id} className="sr-only">Not secure</span>
      </>
    );
  }
  return <Search className={cn(className, "text-muted-foreground")} />;
}

export function BrowserNoticeRow({
  message,
  actionLabel,
  onAction,
  onDismiss,
}: {
  message: string;
  actionLabel?: string;
  onAction?: () => void;
  onDismiss: () => void;
}) {
  return (
    <div className="flex min-h-8 shrink-0 items-center gap-2 border-b bg-warning-background px-3 py-1 text-xs text-warning-foreground">
      <AlertTriangle className="size-3.5 shrink-0" />
      <span className="min-w-0 flex-1 truncate">{message}</span>
      {actionLabel && onAction && (
        <button
          type="button"
          className={cn(
            "shrink-0 rounded px-1.5 py-0.5 font-medium underline underline-offset-2",
            FOCUS_RING_TIGHT,
            HOVER_TINT,
          )}
          onClick={onAction}
        >
          {actionLabel}
        </button>
      )}
      <button
        type="button"
        className={cn(
          "grid size-5 shrink-0 place-items-center rounded",
          FOCUS_RING_TIGHT,
          HOVER_TINT,
        )}
        onClick={onDismiss}
      >
        <X className="size-3" />
        <span className="sr-only">Dismiss</span>
      </button>
    </div>
  );
}
