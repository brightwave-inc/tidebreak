import {
  CircleAlert,
  CircleCheck,
  Info,
  RotateCw,
  TriangleAlert,
  type LucideIcon,
} from "lucide-react";
import {
  isValidElement,
  type HTMLAttributes,
  type ReactElement,
  type ReactNode,
} from "react";

import { Button, type ButtonProps } from "@/components/ui/button";
import { cn } from "@/lib/utils";

/** The status vocabulary from DESIGN.md, plus `neutral` for plain notes. */
export type NoticeTone =
  | "critical"
  | "warning"
  | "info"
  | "success"
  | "neutral";

const TONES: Record<
  NoticeTone,
  {
    edge: string | null;
    mark: string;
    icon: LucideIcon;
    role: "alert" | "status";
  }
> = {
  critical: {
    edge: "bg-critical",
    mark: "text-critical",
    icon: CircleAlert,
    role: "alert",
  },
  warning: {
    edge: "bg-warning",
    mark: "text-warning",
    icon: TriangleAlert,
    role: "status",
  },
  info: {
    edge: "bg-info",
    mark: "text-info",
    icon: Info,
    role: "status",
  },
  success: {
    edge: "bg-success",
    mark: "text-success",
    icon: CircleCheck,
    role: "status",
  },
  neutral: {
    // A plain note keeps the hairline alone.
    edge: null,
    mark: "text-muted-foreground",
    icon: Info,
    role: "status",
  },
};

export type NoticeProps = Omit<HTMLAttributes<HTMLDivElement>, "title"> & {
  /** What the notice is about. Defaults to `neutral`. */
  tone?: NoticeTone;
  /** A short verdict in sentence case. The body says what to do next. */
  title?: ReactNode;
  /** The action slot: a Retry, a link to settings, a dismiss. */
  action?: ReactNode;
  /**
   * Replaces the tone's icon; `null` draws none. An element (a live loader
   * while the work runs) takes the icon's place as it is.
   */
  icon?: LucideIcon | ReactElement | null;
  /**
   * Docks the notice to the top or bottom edge of a pane, as a strip across
   * it: square corners, no side border, and one hairline where it meets the
   * pane's content. The tone keeps its leading edge.
   */
  docked?: "top" | "bottom";
  /** `compact` sets the notice in the dense chrome size (`text-xs`). */
  density?: "default" | "compact";
};

/**
 * The one shape a notice or a failure takes (see DESIGN.md, Notices and
 * errors): a neutral surface with a hairline border, the tone on its leading
 * edge and icon, the message in normal ink, and an action slot.
 *
 * The leading edge is a straight bar laid over the hairline between the
 * rounded corners, so it never bends around them into a bracket.
 *
 * The notice owns the whole shape, so a caller never draws a radius, a
 * border, or padding around a message itself. `className` places the notice
 * (margins, width); it does not restyle it.
 *
 * A panel-level failure is a critical notice whose action retries the load.
 * An empty index is not a failure: it keeps `EmptyMedia variant="icon"`.
 */
export function Notice({
  tone = "neutral",
  title,
  action,
  icon,
  docked,
  density = "default",
  className,
  children,
  role,
  ...props
}: NoticeProps) {
  const toneStyle = TONES[tone];
  const Icon = icon === undefined ? toneStyle.icon : icon;
  const compact = density === "compact";
  return (
    <div
      data-slot="notice"
      data-tone={tone}
      role={role ?? toneStyle.role}
      className={cn(
        // The icon and message travel together, so the icon stays on the
        // first line of text however tall the action beside it is. The action
        // sits beside the message while the message keeps about 16rem, and
        // wraps under it, in line with the text, in a narrow panel. A flex
        // wrap rather than a container query, so a notice can still size to
        // its content.
        "relative flex w-full min-w-0 flex-wrap items-center gap-x-4 gap-y-2 border border-border bg-page-background/70 text-foreground",
        compact ? "px-2.5 py-1.5 text-xs" : "px-3 py-2.5 text-sm",
        docked
          ? cn(
              "rounded-none border-x-0",
              docked === "top" ? "border-t-0" : "border-b-0",
            )
          : compact
            ? "rounded-md"
            : "rounded-lg",
        className,
      )}
      {...props}
    >
      {toneStyle.edge && (
        <span
          aria-hidden="true"
          data-slot="notice-edge"
          className={cn(
            "pointer-events-none absolute w-[2px]",
            // Between the corners' curves, so the bar stays straight.
            docked
              ? "inset-y-0 start-0"
              : compact
                ? "inset-y-[4px] -start-px rounded-full"
                : "inset-y-[7px] -start-px rounded-full",
            toneStyle.edge,
          )}
        />
      )}
      <div
        className={cn(
          "flex min-w-0 flex-[1_1_16rem] items-start",
          compact ? "gap-2" : "gap-2.5",
        )}
      >
        {isValidElement(Icon) ? (
          <span
            aria-hidden="true"
            className={cn(
              "mt-px flex shrink-0 items-center justify-center",
              compact ? "size-3.5" : "size-4",
            )}
          >
            {Icon}
          </span>
        ) : (
          Icon && (
            <Icon
              aria-hidden="true"
              className={cn(
                "mt-px shrink-0",
                compact ? "size-3.5" : "size-4",
                toneStyle.mark,
              )}
            />
          )
        )}
        <div className="min-w-0 flex-1 text-pretty [overflow-wrap:anywhere]">
          {title && <div className="font-medium text-foreground">{title}</div>}
          {children != null && children !== false && children !== "" && (
            <div
              className={cn(
                title ? "mt-0.5 text-muted-foreground" : "text-foreground",
              )}
            >
              {children}
            </div>
          )}
        </div>
      </div>
      {action && (
        <div
          data-slot="notice-action"
          className={cn(
            "flex shrink-0 flex-wrap items-center gap-2",
            // Past the icon column, so a wrapped action lines up with the text.
            Icon && (compact ? "ms-[22px]" : "ms-[26px]"),
          )}
        >
          {action}
        </div>
      )}
    </div>
  );
}

/**
 * Technical detail under a notice's message: an error the server wrote, a
 * command's output. Set in the machine's voice so it reads as evidence rather
 * than as the recovery.
 */
export function NoticeDetail({
  className,
  ...props
}: HTMLAttributes<HTMLElement>) {
  return (
    <code
      data-slot="notice-detail"
      className={cn(
        "mt-1.5 block w-fit max-w-full rounded bg-muted px-1.5 py-0.5 font-mono text-xs text-foreground [overflow-wrap:anywhere] whitespace-pre-wrap",
        className,
      )}
      {...props}
    />
  );
}

/**
 * The Retry a failure offers: it re-runs whatever failed to load. Wire it to
 * the panel's own reload or refetch, never to a page reload.
 */
export function NoticeRetryButton({
  children = "Try again",
  size = "sm",
  variant = "outline",
  ...props
}: ButtonProps) {
  return (
    <Button type="button" size={size} variant={variant} {...props}>
      <RotateCw aria-hidden="true" />
      {children}
    </Button>
  );
}
