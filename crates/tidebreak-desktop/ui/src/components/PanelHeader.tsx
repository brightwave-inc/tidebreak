import type { ComponentProps, ReactNode } from "react";
import { Maximize2, Minimize2, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

/**
 * Every panel wears the same two header rows: chrome on top (where it sits and
 * how to get rid of it), identity and actions below. Keeping them as primitives
 * rather than per-panel markup is what makes one panel feel like the next.
 *
 * Fullscreen and close arrive as callbacks rather than being read from the
 * layout here, so a panel body can be rendered and tested without the routing
 * that decides where it lives.
 */

interface PanelPrimaryHeaderProps extends ComponentProps<"div"> {
  /** Parent-and-current trail, e.g. "Sources / Quarterly report.pdf". */
  breadcrumb?: ReactNode;
  /** Controls before the breadcrumb. */
  leftSlot?: ReactNode;
  /** Controls after the breadcrumb, before the chrome buttons. */
  rightSlot?: ReactNode;
  /** Push the chrome buttons to opposite ends. Suits list panels with no breadcrumb. */
  spaceBetween?: boolean;
  showBorder?: boolean;
  isFullscreen?: boolean;
  /** Omit to hide the fullscreen control. */
  onToggleFullscreen?: () => void;
  /** Omit to hide the close control. */
  onClose?: () => void;
}

export function PanelPrimaryHeader({
  breadcrumb,
  leftSlot,
  rightSlot,
  spaceBetween = false,
  showBorder = false,
  isFullscreen = false,
  onToggleFullscreen,
  onClose,
  className,
  ...props
}: PanelPrimaryHeaderProps) {
  const fullscreenLabel = isFullscreen ? "Exit fullscreen" : "Fullscreen";
  const fullscreenButton = onToggleFullscreen ? (
    <WithTooltip label={fullscreenLabel}>
      <Button variant="ghost" size="icon-sm" onClick={onToggleFullscreen}>
        {isFullscreen ? <Minimize2 className="size-4" /> : <Maximize2 className="size-4" />}
        <span className="sr-only">{fullscreenLabel}</span>
      </Button>
    </WithTooltip>
  ) : null;

  return (
    <div
      className={cn(
        "flex h-11.25 shrink-0 items-center gap-2 p-1",
        !breadcrumb && spaceBetween && "justify-between",
        showBorder && "border-b",
        className,
      )}
      {...props}
    >
      {spaceBetween && fullscreenButton}
      {breadcrumb && <div className="flex min-w-0 items-center pl-2">{breadcrumb}</div>}
      {leftSlot}
      <div className="flex-1" />
      {rightSlot}
      {!spaceBetween && fullscreenButton}
      {onClose && (
        <WithTooltip label="Close">
          <Button variant="ghost" size="icon-sm" onClick={onClose}>
            <X className="size-4" />
            <span className="sr-only">Close</span>
          </Button>
        </WithTooltip>
      )}
    </div>
  );
}

interface PanelSecondaryHeaderProps extends ComponentProps<"div"> {
  showBorder?: boolean;
}

/** The panel's own title row: what it is, and what can be done to it. */
export function PanelSecondaryHeader({
  children,
  className,
  showBorder = true,
  ...props
}: PanelSecondaryHeaderProps) {
  return (
    <div
      className={cn(
        "flex h-11.25 shrink-0 items-center gap-2 bg-transparent",
        showBorder && "border-b",
        className,
      )}
      {...props}
    >
      {children}
    </div>
  );
}

/** "Parent / current" trail for a panel that drilled into one of its items. */
export function PanelBreadcrumb({
  firstPart,
  currentItem,
}: {
  firstPart: ReactNode;
  currentItem?: ReactNode;
}) {
  return (
    <div className="flex min-w-0 items-center gap-2">
      <div className="shrink-0 text-sm font-medium text-muted-foreground">{firstPart}</div>
      {currentItem && (
        <>
          <span className="shrink-0 text-muted-foreground">/</span>
          <div className="min-w-0 truncate text-sm font-medium">{currentItem}</div>
        </>
      )}
    </div>
  );
}
