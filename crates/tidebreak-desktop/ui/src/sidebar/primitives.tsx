import { forwardRef, useState, type ComponentProps } from "react";
import { Slot } from "@radix-ui/react-slot";

import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { useUiStore } from "@/UiStore";

/**
 * The navigation rail, in two widths.
 *
 * Compact keeps the rail rather than hiding it: a sidebar that disappears takes
 * the way back with it, and the reader is left hunting for a control to bring
 * it back. Collapsed here means icons only, still reachable, with each item's
 * name in a tooltip.
 */
export type SidebarWidth = "compact" | "expanded";

export function useSidebarWidth(): SidebarWidth {
  return useUiStore((state) => (state.sidebarCollapsed ? "compact" : "expanded"));
}

export function Sidebar({ className, children, ...props }: ComponentProps<"div">) {
  const width = useSidebarWidth();
  const isCompact = width === "compact";

  return (
    <TooltipProvider>
      <div
        {...props}
        className={cn(
          "relative flex flex-col overflow-hidden transition-all duration-200",
          isCompact && "shrink-0",
          className,
        )}
        style={{
          flex: isCompact ? "0 0 var(--sidebar-compact-width)" : "var(--sidebar-expanded-flex)",
          // Above 1600px, --sidebar-expanded-flex sets flex-shrink to 1 so the
          // rail can grow with the window. Without a min-width floor, that same
          // shrink lets a second open content panel squeeze the rail down to a
          // sliver instead of taking space from the panels themselves.
          minWidth: isCompact ? "var(--sidebar-compact-width)" : "var(--sidebar-expanded-min)",
        }}
        data-sidebar={width}
      >
        {children}
      </div>
    </TooltipProvider>
  );
}

export function SidebarHeader({
  asChild = false,
  className,
  ...props
}: ComponentProps<"div"> & { asChild?: boolean }) {
  const Comp = asChild ? Slot : "div";
  return (
    <Comp className={cn("mt-2 flex h-9 shrink-0 items-center gap-2 px-2", className)} {...props} />
  );
}

export const SidebarContent = forwardRef<
  HTMLDivElement,
  ComponentProps<"div"> & { asChild?: boolean }
>(function SidebarContent({ asChild = false, className, ...props }, ref) {
  const Comp = asChild ? Slot : "div";
  return <Comp ref={ref} className={cn("mt-4 flex grow flex-col", className)} {...props} />;
});

export function SidebarFooter({
  asChild = false,
  className,
  ...props
}: ComponentProps<"div"> & { asChild?: boolean }) {
  const Comp = asChild ? Slot : "div";
  return <Comp className={cn("shrink-0 p-2", className)} {...props} />;
}

export function SidebarSectionTitle({ className, ...props }: ComponentProps<"div">) {
  const isCompact = useSidebarWidth() === "compact";
  return (
    <div
      className={cn(
        "line-clamp-1 shrink-0 truncate px-2 py-1 text-sm font-medium text-muted-foreground opacity-100 transition-opacity",
        isCompact && "opacity-0",
        className,
      )}
      {...props}
    />
  );
}

/**
 * A row in the rail. Compact hides everything but the leading icon and moves
 * the label — taken from the first `span` — into a tooltip, so one definition
 * serves both widths.
 */
export function SidebarButton({
  asChild = false,
  children,
  className,
  ...props
}: ComponentProps<"button"> & { asChild?: boolean }) {
  const Comp = asChild ? Slot : "button";
  const [label, setLabel] = useState("");
  const isCompact = useSidebarWidth() === "compact";

  const measureLabel = (element: HTMLButtonElement | null) => {
    if (!element) return;
    setLabel(element.querySelector("span")?.textContent ?? "");
  };

  const button = (
    <Comp
      ref={measureLabel}
      className={cn(
        "inline-flex w-full cursor-pointer items-center gap-2 rounded-md p-2 text-left text-sm font-[450] whitespace-nowrap ring-offset-background transition-colors hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:outline-none disabled:pointer-events-none disabled:opacity-50 [&_svg]:size-4 [&_svg]:shrink-0",
        isCompact && "justify-center [&>*:not(:first-child)]:hidden",
        className,
      )}
      {...props}
    >
      {children}
    </Comp>
  );

  if (!isCompact) return button;

  return (
    <Tooltip delayDuration={150}>
      <TooltipTrigger asChild>{button}</TooltipTrigger>
      <TooltipContent side="right" align="center">
        {label}
      </TooltipContent>
    </Tooltip>
  );
}
