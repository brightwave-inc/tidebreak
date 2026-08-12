import { PanelResizeHandle } from "react-resizable-panels";

import { cn } from "@/lib/utils";

/**
 * A hairline that thickens under the cursor, with a grip pill riding on it. It
 * stays in the tree when disabled rather than unmounting, so the panel widths
 * either side do not jump as slots open and close.
 */
export function PanelDragHandle({
  disabled,
  onDragging,
}: {
  disabled?: boolean;
  onDragging?: (dragging: boolean) => void;
}) {
  return (
    <PanelResizeHandle
      className={cn(
        "group relative z-20 -mr-1 grid h-full w-2 place-items-center transition-opacity max-md:hidden",
        disabled ? "pointer-events-none opacity-0" : "cursor-col-resize",
      )}
      disabled={disabled}
      onDragging={onDragging}
    >
      <div className="absolute top-0 right-1 bottom-0 w-[0.5px] bg-border transition-all group-hover:w-px group-hover:translate-x-[0.5px] group-hover:bg-foreground/35 group-hover:delay-75 group-data-[resize-handle-state=drag]:bg-foreground/75" />
      <div className="relative h-6 w-2 cursor-col-resize rounded-full border-[0.5px] border-border bg-background shadow transition duration-200 group-hover:border-foreground/10 group-hover:bg-muted group-hover:delay-75 group-data-[resize-handle-state=drag]:bg-foreground" />
    </PanelResizeHandle>
  );
}
