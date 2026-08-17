import {
  forwardRef,
  useCallback,
  useEffect,
  useRef,
  useState,
  type ComponentProps,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { Slot } from "@radix-ui/react-slot";

import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import {
  SIDEBAR_DEFAULT_WIDTH,
  SIDEBAR_MAX_WIDTH,
  SIDEBAR_MIN_WIDTH,
  useUiStore,
} from "@/UiStore";

/**
 * The navigation rail, in two widths.
 *
 * Compact keeps the rail rather than hiding it: a sidebar that disappears takes
 * the way back with it, and the reader is left hunting for a control to bring
 * it back. Collapsed here means icons only, still reachable, with each item's
 * name in a tooltip. Expanded width is reader-chosen and remembered.
 */
export type SidebarWidth = "compact" | "expanded";

export function useSidebarWidth(): SidebarWidth {
  return useUiStore((state) => (state.sidebarCollapsed ? "compact" : "expanded"));
}

/**
 * Publish the expanded rail width onto the shell so chrome that sits over the
 * rail (the titlebar) can track it without reading the store itself.
 */
function useSyncSidebarWidthCssVar(widthPx: number, isCompact: boolean) {
  useEffect(() => {
    const shell = document.querySelector<HTMLElement>(".app-shell");
    if (!shell) return;
    if (isCompact) {
      shell.style.removeProperty("--sidebar-expanded-width");
      return;
    }
    shell.style.setProperty("--sidebar-expanded-width", `${widthPx}px`);
  }, [widthPx, isCompact]);
}

/**
 * Drag handle on the rail's trailing edge. Double-click restores the default
 * width. Lives outside react-resizable-panels so the rail stays independent of
 * the conversation/panel split beside it.
 */
function SidebarResizeHandle({
  onDraggingChange,
}: {
  onDraggingChange: (dragging: boolean) => void;
}) {
  const sidebarWidth = useUiStore((state) => state.sidebarWidth);
  const setSidebarWidth = useUiStore((state) => state.setSidebarWidth);
  const dragRef = useRef<{ pointerId: number; startX: number; startWidth: number } | null>(
    null,
  );
  const [dragging, setDragging] = useState(false);

  const endDrag = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      const drag = dragRef.current;
      if (!drag || drag.pointerId !== event.pointerId) return;
      dragRef.current = null;
      setDragging(false);
      onDraggingChange(false);
      // Commit the live width once; moves during the drag skipped persistence.
      setSidebarWidth(useUiStore.getState().sidebarWidth);
      try {
        event.currentTarget.releasePointerCapture(event.pointerId);
      } catch {
        // Capture may already be released.
      }
      document.body.style.removeProperty("cursor");
      document.body.style.removeProperty("user-select");
    },
    [onDraggingChange, setSidebarWidth],
  );

  const onPointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.preventDefault();
    dragRef.current = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startWidth: sidebarWidth,
    };
    setDragging(true);
    onDraggingChange(true);
    event.currentTarget.setPointerCapture(event.pointerId);
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
  };

  const onPointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    setSidebarWidth(drag.startWidth + (event.clientX - drag.startX), { persist: false });
  };

  const onDoubleClick = () => {
    setSidebarWidth(SIDEBAR_DEFAULT_WIDTH);
  };

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize sidebar"
      aria-valuemin={SIDEBAR_MIN_WIDTH}
      aria-valuemax={SIDEBAR_MAX_WIDTH}
      aria-valuenow={sidebarWidth}
      tabIndex={0}
      className="group absolute top-0 right-0 z-20 hidden h-full w-2 translate-x-1/2 cursor-col-resize touch-none md:block"
      data-dragging={dragging || undefined}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
      onDoubleClick={onDoubleClick}
      onKeyDown={(event) => {
        const step = event.shiftKey ? 32 : 16;
        if (event.key === "ArrowLeft") {
          event.preventDefault();
          setSidebarWidth(sidebarWidth - step);
        } else if (event.key === "ArrowRight") {
          event.preventDefault();
          setSidebarWidth(sidebarWidth + step);
        } else if (event.key === "Home") {
          event.preventDefault();
          setSidebarWidth(SIDEBAR_MIN_WIDTH);
        } else if (event.key === "End") {
          event.preventDefault();
          setSidebarWidth(SIDEBAR_MAX_WIDTH);
        }
      }}
    >
      <div className="absolute top-0 right-1 bottom-0 w-[0.5px] bg-border transition-all group-hover:w-px group-hover:translate-x-[0.5px] group-hover:bg-foreground/35 group-hover:delay-75 group-data-[dragging]:bg-foreground/75" />
      <div className="absolute top-1/2 right-0 h-6 w-2 -translate-y-1/2 cursor-col-resize rounded-full border-[0.5px] border-border bg-background opacity-0 shadow transition duration-200 group-hover:opacity-100 group-hover:delay-75 group-data-[dragging]:opacity-100 group-data-[dragging]:border-foreground/10 group-data-[dragging]:bg-foreground" />
    </div>
  );
}

export function Sidebar({ className, children, ...props }: ComponentProps<"div">) {
  const width = useSidebarWidth();
  const isCompact = width === "compact";
  const sidebarWidthPx = useUiStore((state) => state.sidebarWidth);
  const [resizing, setResizing] = useState(false);
  useSyncSidebarWidthCssVar(sidebarWidthPx, isCompact);

  return (
    <TooltipProvider>
      <div
        {...props}
        className={cn(
          "relative flex shrink-0 flex-col overflow-hidden",
          // Animate collapse/expand, but not live drags — easing lags the pointer.
          !isCompact && !resizing && "transition-[flex-basis,min-width,width] duration-200",
          className,
        )}
        style={{
          flex: isCompact
            ? "0 0 var(--sidebar-compact-width)"
            : `0 0 ${sidebarWidthPx}px`,
          minWidth: isCompact ? "var(--sidebar-compact-width)" : `${sidebarWidthPx}px`,
          width: isCompact ? undefined : sidebarWidthPx,
        }}
        data-sidebar={width}
      >
        {children}
        {!isCompact && <SidebarResizeHandle onDraggingChange={setResizing} />}
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
