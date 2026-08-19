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

import { TooltipProvider } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import {
  SIDEBAR_DEFAULT_WIDTH,
  SIDEBAR_MAX_WIDTH,
  SIDEBAR_MIN_WIDTH,
  useUiStore,
} from "@/UiStore";

/**
 * The navigation rail, shown or gone.
 *
 * When collapsed the rail leaves the layout entirely; the shell keeps a single
 * expand button pinned beside the window's traffic lights so the way back
 * never disappears with the rail. Expanded width is reader-chosen and
 * remembered.
 */

/**
 * Publish the expanded rail width onto the shell so the overlay titlebar can
 * size itself to the rail. Cleared on collapse: the titlebar unmounts with the
 * rail, and the fallback 280px would otherwise paint leftover chrome.
 */
export function useSyncSidebarWidthCssVar() {
  const sidebarWidthPx = useUiStore((state) => state.sidebarWidth);
  const collapsed = useUiStore((state) => state.sidebarCollapsed);
  useEffect(() => {
    const shell = document.querySelector<HTMLElement>(".app-shell");
    if (!shell) return;
    if (collapsed) {
      shell.style.removeProperty("--sidebar-expanded-width");
      return;
    }
    shell.style.setProperty("--sidebar-expanded-width", `${sidebarWidthPx}px`);
  }, [sidebarWidthPx, collapsed]);
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
  const collapsed = useUiStore((state) => state.sidebarCollapsed);
  const sidebarWidthPx = useUiStore((state) => state.sidebarWidth);
  const [resizing, setResizing] = useState(false);
  if (collapsed) return null;

  return (
    <TooltipProvider>
      <div
        {...props}
        className={cn(
          "relative flex shrink-0 flex-col overflow-hidden",
          // Animate width changes, but not live drags — easing lags the pointer.
          !resizing && "transition-[flex-basis,min-width,width] duration-200",
          className,
        )}
        style={{
          flex: `0 0 ${sidebarWidthPx}px`,
          minWidth: `${sidebarWidthPx}px`,
          width: sidebarWidthPx,
        }}
        data-sidebar="expanded"
      >
        {children}
        <SidebarResizeHandle onDraggingChange={setResizing} />
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
  return (
    <div
      className={cn(
        "line-clamp-1 shrink-0 truncate px-2 py-1 text-sm font-medium text-muted-foreground",
        className,
      )}
      {...props}
    />
  );
}

export function SidebarButton({
  asChild = false,
  className,
  ...props
}: ComponentProps<"button"> & { asChild?: boolean }) {
  const Comp = asChild ? Slot : "button";
  return (
    <Comp
      className={cn(
        "inline-flex w-full cursor-pointer items-center gap-2 rounded-md p-2 text-left text-sm font-[450] whitespace-nowrap ring-offset-background transition-colors hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:outline-none disabled:pointer-events-none disabled:opacity-50 [&_svg]:size-4 [&_svg]:shrink-0",
        className,
      )}
      {...props}
    />
  );
}
