import { useId, useState, type ReactNode } from "react";
import { ChevronDown } from "lucide-react";

import { cn } from "@/lib/utils";
import { FOCUS_RING_TIGHT, HOVER_TINT } from "@/code/interactive";

type ToolCardShellProps = {
  /** What ran, drawn from the tool's allowlisted name. */
  icon: ReactNode;
  /** The one line the card is worth reading for. */
  title: ReactNode;
  /** Monospace when the title is a command rather than prose. */
  titleClassName?: string;
  /** Secondary execution facts, shown with the expanded detail. */
  badge?: ReactNode;
  /** Compact metadata that belongs on the collapsed row. */
  trailing?: ReactNode;
  /** Open on mount, which is worth doing only while work is still happening. */
  defaultExpanded?: boolean;
  /** Controlled expansion for hosts that already own reveal state. */
  expanded?: boolean;
  onExpandedChange?: (expanded: boolean) => void;
  /** Extra row-level classes. */
  className?: string;
  bodyClassName?: string;
  children: ReactNode;
  /** Assistive label; the visible title is usually not the whole sentence. */
  label: string;
  /** Work/chat tool rows announce status changes; code-mode rows do not. */
  announce?: boolean;
};

/**
 * The shared expandable tool row used by work/chat and code mode.
 *
 * The collapsed state is deliberately boxless: a transcript should read as a
 * conversation with occasional notes about what ran, not as a stack of nested
 * panels. Provider and outcome metadata live with the expanded detail unless a
 * host supplies genuinely glanceable trailing metadata, such as code mode's
 * duration and status glyph.
 */
export function ToolCardShell({
  icon,
  title,
  titleClassName,
  badge,
  trailing,
  defaultExpanded = false,
  expanded: controlledExpanded,
  onExpandedChange,
  className,
  bodyClassName,
  children,
  label,
  announce = true,
}: ToolCardShellProps) {
  const [uncontrolledExpanded, setUncontrolledExpanded] =
    useState(defaultExpanded);
  const expanded = controlledExpanded ?? uncontrolledExpanded;
  const bodyId = useId();

  function setExpanded(expanded: boolean) {
    if (controlledExpanded === undefined) setUncontrolledExpanded(expanded);
    onExpandedChange?.(expanded);
  }

  return (
    <section
      className={cn("max-w-prose [overflow-anchor:none]", className)}
      aria-label={label}
      role={announce ? "status" : undefined}
      aria-live={announce ? "polite" : undefined}
      aria-atomic={announce ? "true" : undefined}
    >
      <button
        type="button"
        className={cn(
          "-mx-1.5 flex w-full cursor-pointer items-center gap-2 rounded-md px-1.5 py-0.5 text-left text-[13.5px] hover:bg-muted/50",
          FOCUS_RING_TIGHT,
          HOVER_TINT,
        )}
        aria-expanded={expanded}
        aria-controls={bodyId}
        onClick={() => setExpanded(!expanded)}
      >
        <span className="text-muted-foreground shrink-0 [&>svg]:size-3.5">
          {icon}
        </span>
        <span className={cn("min-w-0 flex-1", titleClassName)}>
          {typeof title === "string" ? (
            <span className="block truncate" title={title}>
              {title}
            </span>
          ) : (
            // Rendered directly so a `titleClassName` flex layout reaches the
            // host's own title children; an inline wrapper here forces a
            // block-level child (e.g. MiddleTruncate) onto its own line.
            title
          )}
        </span>
        <span className="text-muted-foreground ml-auto flex shrink-0 items-center gap-1.5 text-[11px] tabular-nums">
          {trailing}
          <ChevronDown
            className={cn(
              "text-muted-foreground/50 size-3.5 transition-transform duration-[140ms] ease-out motion-reduce:transition-none",
              !expanded && "-rotate-90",
            )}
            aria-hidden="true"
          />
        </span>
      </button>
      {expanded && (
        <div id={bodyId} className={cn("mt-1.5", bodyClassName)}>
          {badge && (
            <div className="mb-1.5 flex flex-wrap items-center gap-1">
              {badge}
            </div>
          )}
          {children}
        </div>
      )}
    </section>
  );
}
