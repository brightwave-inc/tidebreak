import { useId, useState, type ReactNode } from "react";
import { ChevronDown } from "lucide-react";

import { cn } from "@/lib/utils";

type ToolCardShellProps = {
  /** What ran, drawn from the tool's allowlisted name. */
  icon: ReactNode;
  /** The one line the card is worth reading for. */
  title: ReactNode;
  /** Monospace when the title is a command rather than prose. */
  titleClassName?: string;
  /** The outcome, always visible — a collapsed card still has to report. */
  badge: ReactNode;
  /** Open on mount, which is worth doing only while work is still happening. */
  defaultExpanded?: boolean;
  /** Extra card-level classes, e.g. a failure tint on the border. */
  className?: string;
  children: ReactNode;
  /** Assistive label; the visible title is usually not the whole sentence. */
  label: string;
};

/**
 * The chrome every tool-result card shares: one header line that names what ran
 * and how it ended, over a body the reader opens when they want it.
 *
 * A transcript should read as a conversation with occasional notes about what
 * the agent did, not as a log. So the default is one line, the outcome stays on
 * that line, and the detail is one click away — a stream of tool calls stays
 * scannable instead of flooding the conversation with output.
 */
export function ToolCardShell({
  icon,
  title,
  titleClassName,
  badge,
  defaultExpanded = false,
  className,
  children,
  label,
}: ToolCardShellProps) {
  const [expanded, setExpanded] = useState(defaultExpanded);
  const bodyId = useId();

  return (
    <section
      className={cn(
        "bg-background max-w-prose overflow-hidden rounded-lg border",
        className,
      )}
      aria-label={label}
      role="status"
      aria-live="polite"
      aria-atomic="true"
    >
      <button
        type="button"
        className="hover:bg-muted/50 focus-visible:ring-ring flex w-full items-center justify-between gap-2 px-2.5 py-1.5 text-left transition-colors focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:outline-hidden"
        aria-expanded={expanded}
        aria-controls={bodyId}
        onClick={() => setExpanded((current) => !current)}
      >
        <span className="text-muted-foreground flex min-w-0 items-center gap-1.5 text-xs font-medium">
          <ChevronDown
            className={cn(
              "size-3.5 shrink-0 transition-transform",
              !expanded && "-rotate-90",
            )}
            aria-hidden="true"
          />
          {icon}
          <span className={cn("truncate", titleClassName)}>{title}</span>
        </span>
        {badge}
      </button>
      {expanded && (
        <div id={bodyId} className="border-t">
          {children}
        </div>
      )}
    </section>
  );
}
