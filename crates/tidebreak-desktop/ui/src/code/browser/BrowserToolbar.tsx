import {
  cloneElement,
  isValidElement,
  useEffect,
  useId,
  useRef,
  useState,
  type FormEvent,
  type ReactElement,
  type ReactNode,
  type RefObject,
} from "react";
import {
  AlertTriangle,
  ArrowLeft,
  ArrowRight,
  Bot,
  ExternalLink,
  Hand,
  History,
  Info,
  Laptop,
  LoaderCircle,
  LockKeyhole,
  Pause,
  RefreshCw,
  Search,
  ShieldCheck,
  ShieldAlert,
  X,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { FOCUS_RING_TIGHT, HOVER_TINT } from "../interactive";
import type {
  BrowserAgentAccess,
  BrowserController,
  BrowserHostSnapshot,
} from "./browserHost";
import { browserSecurity, MAX_BROWSER_URL_CHARS } from "./browserNavigation";
import type { BrowserSession } from "./browserSession";

type BrowserEngine = NonNullable<BrowserHostSnapshot["engine"]>;

const COMPACT_TOOLBAR_WIDTH = 420;

/**
 * Observes a container element and returns true while its inline-size
 * is less than `breakpoint`.  False for zero-size or disconnected elements.
 */
function useCompactToolbar(
  ref: RefObject<HTMLDivElement | null>,
  breakpoint: number,
): boolean {
  const [compact, setCompact] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const check = () => {
      const width = el.clientWidth;
      setCompact(width > 0 && width < breakpoint);
    };
    const observer = new ResizeObserver(check);
    observer.observe(el);
    check();
    return () => observer.disconnect();
  }, [ref, breakpoint]);
  return compact;
}

export function BrowserToolbar({
  session,
  address,
  addressError,
  canGoBack,
  canGoForward,
  controller,
  agentAccess,
  engine,
  onAddressChange,
  onNavigate,
  onBack,
  onForward,
  onReload,
  onStop,
  onStopAgent,
  onTakeOver,
  onShareAgent,
  onRevokeAgent,
  onSelectHistory,
  onOpenExternal,
  onOverlayOpenChange,
  onAgentAccessOpenChange,
  agentAccessOpen = false,
  viewportControl,
}: {
  session: BrowserSession;
  address: string;
  addressError: string | null;
  canGoBack: boolean;
  canGoForward: boolean;
  controller?: BrowserController;
  agentAccess?: BrowserAgentAccess;
  engine?: BrowserEngine;
  onAddressChange: (value: string) => void;
  onNavigate: () => void;
  onBack: () => void;
  onForward: () => void;
  onReload: () => void;
  onStop: () => void;
  onStopAgent?: () => void;
  onTakeOver?: () => void;
  onShareAgent?: () => void;
  onRevokeAgent?: () => void;
  onSelectHistory: (index: number) => void;
  onOpenExternal: () => void;
  onOverlayOpenChange: (open: boolean) => void;
  onAgentAccessOpenChange?: (open: boolean) => void;
  agentAccessOpen?: boolean;
  viewportControl?: ReactNode;
}) {
  const inputRef = useRef<HTMLInputElement | null>(null);
  const toolbarRef = useRef<HTMLDivElement | null>(null);
  const addressErrorId = useId();
  const securityId = useId();
  const compactToolbar = useCompactToolbar(toolbarRef, COMPACT_TOOLBAR_WIDTH);
  const security = session.url ? browserSecurity(session.url) : null;

  useEffect(() => {
    if (!session.url) inputRef.current?.focus();
  }, [session.url]);

  function submit(event: FormEvent) {
    event.preventDefault();
    onNavigate();
  }

  return (
    <header className="relative z-[1] shrink-0 border-b border-border-subtle bg-page-background/88 backdrop-blur-md">
      <div
        ref={toolbarRef}
        data-compact={compactToolbar || undefined}
        className={cn(
          "flex min-w-0 items-center py-1.5",
          compactToolbar ? "gap-1 px-1.5" : "gap-1.5 px-2",
        )}
      >
        <nav
          aria-label="Browser navigation"
          className="flex shrink-0 items-center rounded-lg border border-border-subtle bg-background/72 p-0.5 shadow-[0_1px_2px_color-mix(in_oklch,var(--foreground)_5%,transparent)]"
        >
          <WithTooltip label="Back">
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              className="rounded-md"
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
              size="icon-xs"
              className="rounded-md"
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
              size="icon-xs"
              className="rounded-md"
              disabled={!session.url}
              onClick={session.loadState === "loading" ? onStop : onReload}
            >
              {session.loadState === "loading" ? <X /> : <RefreshCw />}
              <span className="sr-only">
                {session.loadState === "loading" ? "Stop" : "Reload"}
              </span>
            </Button>
          </WithTooltip>
        </nav>

        <form
          className={cn("flex-1", compactToolbar ? "min-w-20" : "min-w-0")}
          onSubmit={submit}
        >
          <label
            className={cn(
              "flex h-8 min-w-0 items-center rounded-lg border border-border-subtle bg-background/88 text-xs",
              compactToolbar ? "gap-1.5 px-2" : "gap-2 px-2.5",
              "shadow-[inset_0_1px_0_color-mix(in_oklch,var(--foreground)_3%,transparent),0_1px_2px_color-mix(in_oklch,var(--foreground)_4%,transparent)]",
              "transition-[border-color,box-shadow,background-color] duration-150 focus-within:border-ring focus-within:bg-background focus-within:ring-3 focus-within:ring-ring/20",
              addressError && "border-critical-border ring-2 ring-critical/10",
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
              className="min-w-0 flex-1 bg-transparent font-medium text-foreground outline-none placeholder:font-normal placeholder:text-muted-foreground"
              onChange={(event) => onAddressChange(event.target.value)}
              onFocus={(event) => event.currentTarget.select()}
            />
            {session.loadState === "loading" ? (
              <LoaderCircle className="size-3.5 shrink-0 animate-spin text-muted-foreground motion-reduce:animate-none" />
            ) : !compactToolbar ? (
              <Search className="size-3.5 shrink-0 text-muted-foreground/70" />
            ) : null}
          </label>
        </form>

        <BrowserAgentAccessControl
          engine={engine}
          access={agentAccess}
          compact={compactToolbar}
          onShare={onShareAgent}
          onRevoke={onRevokeAgent}
          onAgentAccessOpenChange={onAgentAccessOpenChange}
          agentAccessOpen={agentAccessOpen}
        />

        {viewportControl && (
          <div className="shrink-0">
            {isValidElement(viewportControl)
              ? cloneElement(viewportControl as ReactElement<{ compact?: boolean }>, { compact: compactToolbar })
              : viewportControl}
          </div>
        )}

        <DropdownMenu onOpenChange={onOverlayOpenChange}>
          <WithTooltip label="History">
            <DropdownMenuTrigger asChild>
              <Button
                type="button"
                variant="ghost"
                size={compactToolbar ? "icon-xs" : "icon-sm"}
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
            size={compactToolbar ? "icon-xs" : "icon-sm"}
            disabled={!session.url}
            onClick={onOpenExternal}
          >
            <ExternalLink />
            <span className="sr-only">Open externally</span>
          </Button>
        </WithTooltip>
      </div>

      {addressError && (
        <p
          id={addressErrorId}
          role="alert"
          className="border-t border-critical-border/45 bg-critical-background/55 px-3 py-1 text-[11px] text-critical-foreground"
        >
          {addressError}
        </p>
      )}

      {controller?.kind === "agent" && (
        <BrowserAgentControlRow
          controller={controller}
          onStop={onStopAgent}
          onTakeOver={onTakeOver}
        />
      )}
    </header>
  );
}

function BrowserAgentAccessControl({
  engine,
  access,
  compact = false,
  onShare,
  onRevoke,
  onAgentAccessOpenChange,
  agentAccessOpen = false,
}: {
  engine?: BrowserEngine;
  access?: BrowserAgentAccess;
  compact?: boolean;
  onShare?: () => void;
  onRevoke?: () => void;
  onAgentAccessOpenChange?: (open: boolean) => void;
  agentAccessOpen?: boolean;
}) {
  // Close the compact Agent access menu when the toolbar grows past the
  // compact breakpoint. Radix does not call onOpenChange(false) when its
  // root is conditionally unmounted, so without this the parent visibility
  // source would stay latched true and the native view would stay hidden.
  useEffect(() => {
    if (!compact && agentAccessOpen) {
      onAgentAccessOpenChange?.(false);
    }
  }, [compact, agentAccessOpen, onAgentAccessOpenChange]);

  if (!engine?.capabilities.semanticSnapshot || !access?.origin) return null;
  const originLabel = browserOriginLabel(access.origin);

  if (compact && (access.paused || access.shared)) {
    return (
      <CompactAgentAccessControl
        access={access}
        originLabel={originLabel}
        onShare={onShare}
        onRevoke={onRevoke}
        open={agentAccessOpen}
        onOpenChangeControlled={onAgentAccessOpenChange}
      />
    );
  }

  if (access.paused) {
    return (
      <div
        className="flex h-7 min-w-0 shrink-0 items-center gap-1 rounded-md border border-warning-border/70 bg-warning-background/72 px-1.5 text-[10px] font-medium text-warning-foreground"
        aria-label={`Agent paused before ${access.origin}`}
      >
        <Pause className="size-3 shrink-0" />
        <span className="shrink-0">Agent paused</span>
        <span className="hidden max-w-28 truncate opacity-75 2xl:inline">
          {originLabel}
        </span>
        {onShare && (
          <Button
            type="button"
            variant="secondary"
            size="2xs"
            onClick={onShare}
          >
            Review &amp; resume
          </Button>
        )}
        {access.shared && onRevoke && (
          <WithTooltip label="Stop sharing with agent">
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              className="size-5"
              onClick={onRevoke}
            >
              <X />
              <span className="sr-only">Stop sharing with agent</span>
            </Button>
          </WithTooltip>
        )}
      </div>
    );
  }

  if (access.shared) {
    const sharedLabel = access.scope === "loopback_workspace"
      ? "Local sites shared"
      : `${originLabel} shared`;
    return (
      <div
        className="flex h-7 min-w-0 shrink-0 items-center gap-1 rounded-md bg-success-background/65 px-1.5 text-[10px] font-medium text-success-foreground"
        aria-label={`Shared with agent: ${access.origin}`}
      >
        <ShieldCheck className="size-3 shrink-0" />
        <span className="max-w-28 truncate">{sharedLabel}</span>
        {onRevoke && (
          <Button
            type="button"
            variant="ghost"
            size="2xs"
            className="text-success-foreground hover:bg-success/10 hover:text-success-foreground"
            onClick={onRevoke}
          >
            Stop sharing
          </Button>
        )}
      </div>
    );
  }

  return onShare ? (
    <Button
      type="button"
      variant="outline"
      size={compact ? "icon-xs" : "xs"}
      className="h-7 bg-background/72 text-[10px]"
      aria-label="Share with agent"
      onClick={onShare}
    >
      <Bot />
      {!compact && "Share with agent"}
    </Button>
  ) : null;
}

function CompactAgentAccessControl({
  access,
  originLabel,
  onShare,
  onRevoke,
  open = false,
  onOpenChangeControlled,
}: {
  access: BrowserAgentAccess;
  originLabel: string;
  onShare?: () => void;
  onRevoke?: () => void;
  open?: boolean;
  onOpenChangeControlled?: (open: boolean) => void;
}) {
  const paused = access.paused;
  const statusLabel = paused
    ? `Agent paused before ${access.origin}`
    : `Shared with agent: ${access.origin}`;
  const displayLabel = paused
    ? "Agent paused"
    : access.scope === "loopback_workspace"
      ? "Local sites shared"
      : `${originLabel} shared`;
  const Icon = paused ? Pause : ShieldCheck;

  return (
    <div className="shrink-0">
      <span className="sr-only" role="status">
        {statusLabel}
      </span>
      <DropdownMenu
        open={open}
        onOpenChange={onOpenChangeControlled}
      >
        <WithTooltip label={displayLabel}>
          <DropdownMenuTrigger asChild>
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              className={cn(
                "size-7",
                paused
                  ? "border border-warning-border/70 bg-warning-background/72 text-warning-foreground hover:bg-warning-background hover:text-warning-foreground"
                  : "bg-success-background/65 text-success-foreground hover:bg-success-background hover:text-success-foreground",
              )}
              aria-label={statusLabel}
            >
              <Icon />
            </Button>
          </DropdownMenuTrigger>
        </WithTooltip>
        <DropdownMenuContent
          align="end"
          aria-label="Agent access"
          className="w-56"
        >
          <DropdownMenuItem
            disabled
            className="gap-2 data-disabled:opacity-100"
          >
            <Icon />
            <span className="min-w-0">
              <span className="block text-xs font-medium">{displayLabel}</span>
              <span className="block truncate text-[11px] text-muted-foreground">
                {originLabel}
              </span>
            </span>
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          {paused && onShare && (
            <DropdownMenuItem onSelect={onShare}>
              <Bot />
              Review &amp; resume
            </DropdownMenuItem>
          )}
          {access.shared && onRevoke && (
            <DropdownMenuItem variant="destructive" onSelect={onRevoke}>
              <X />
              Stop sharing
            </DropdownMenuItem>
          )}
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

function browserOriginLabel(origin: string): string {
  try {
    return new URL(origin).host || origin;
  } catch {
    return origin;
  }
}

export function BrowserAgentControlRow({
  controller,
  onStop,
  onTakeOver,
}: {
  controller: Extract<BrowserController, { kind: "agent" }>;
  onStop?: () => void;
  onTakeOver?: () => void;
}) {
  const halted = Boolean(controller.halted);
  const takeoverRequired = Boolean(controller.takeoverRequired);
  const label = controller.label || "Agent";
  const status = takeoverRequired
    ? "Waiting for you"
    : halted
      ? "Control stopped"
      : `${label} is using this tab`;
  const detail = takeoverRequired
    ? controller.action || "A sensitive field needs human input"
    : halted
      ? "No more browser actions will run until control is explicitly resumed"
      : controller.action || "Inspecting the current page";

  return (
    <div
      className={cn(
        "flex min-h-9 items-center gap-2 border-t px-3 py-1.5 text-xs",
        takeoverRequired
          ? "border-warning-border bg-warning-background text-warning-foreground"
          : halted
            ? "border-border-subtle bg-muted/55 text-muted-foreground"
            : "border-info-border/55 bg-info-background/55 text-info-foreground",
      )}
      role="status"
    >
      <span className="relative grid size-5 shrink-0 place-items-center rounded-md bg-background/60">
        {takeoverRequired ? <Hand className="size-3.5" /> : <Bot className="size-3.5" />}
        {!halted && !takeoverRequired && (
          <span className="absolute -right-0.5 -top-0.5 size-1.5 rounded-full bg-info ring-2 ring-info-background" />
        )}
      </span>
      <span className="min-w-0 flex-1">
        <span className="font-medium text-foreground">{status}</span>
        <span className="ml-2 hidden truncate text-[11px] opacity-80 sm:inline">
          {detail}
        </span>
      </span>
      {!halted && onStop && (
        <Button
          type="button"
          variant="ghost-destructive"
          size="2xs"
          onClick={onStop}
        >
          <X />
          Stop
        </Button>
      )}
      {onTakeOver && (
        <Button
          type="button"
          variant={takeoverRequired ? "secondary" : "outline"}
          size="2xs"
          onClick={onTakeOver}
        >
          <Hand />
          Take over
        </Button>
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
  tone = "warning",
  actionLabel,
  onAction,
  onDismiss,
}: {
  message: string;
  tone?: "info" | "warning" | "critical";
  actionLabel?: string;
  onAction?: () => void;
  onDismiss: () => void;
}) {
  const Icon = tone === "info" ? Info : AlertTriangle;
  return (
    <div
      className={cn(
        "flex min-h-8 shrink-0 items-center gap-2 border-b px-3 py-1 text-xs",
        tone === "info" && "border-info-border bg-info-background text-info-foreground",
        tone === "warning" && "border-warning-border bg-warning-background text-warning-foreground",
        tone === "critical" && "border-critical-border bg-critical-background text-critical-foreground",
      )}
    >
      <Icon className="size-3.5 shrink-0" />
      <span className="min-w-0 flex-1 truncate">{message}</span>
      {actionLabel && onAction && (
        <button
          type="button"
          className={cn(
            "shrink-0 rounded px-1.5 py-0.5 font-medium underline decoration-current/45 underline-offset-2",
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
