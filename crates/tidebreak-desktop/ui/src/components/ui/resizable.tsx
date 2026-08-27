import { Group, Panel, Separator } from "react-resizable-panels";

import { cn } from "@/lib/utils";

const ResizablePanelGroup = ({
  className,
  orientation = "horizontal",
  ...props
}: React.ComponentProps<typeof Group>) => (
  <Group
    {...props}
    orientation={orientation}
    className={cn(
      "flex h-full w-full",
      orientation === "vertical" && "flex-col",
      className,
    )}
  />
);

const ResizablePanel = Panel;

/**
 * The hairline between two panels, plus a wider invisible grab strip.
 *
 * `aria-orientation` describes the separator's own axis, not the group's: a
 * horizontal group is split by a *vertical* separator. Key the full-width bar
 * off `horizontal` accordingly. Keying it off `vertical` — which reads as the
 * matching word — makes the handle a full-width row in every horizontal
 * group, and the panels beside it collapse to zero.
 */
const RESIZABLE_HANDLE_CLASS =
  "relative flex w-px items-center justify-center bg-border after:absolute after:inset-y-0 after:left-1/2 after:w-1 after:-translate-x-1/2 focus-visible:ring-1 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:outline-hidden aria-[orientation=horizontal]:h-px aria-[orientation=horizontal]:w-full aria-[orientation=horizontal]:after:left-0 aria-[orientation=horizontal]:after:h-1 aria-[orientation=horizontal]:after:w-full aria-[orientation=horizontal]:after:translate-x-0 aria-[orientation=horizontal]:after:-translate-y-1/2";

const ResizableHandle = ({
  className,
  ...props
}: React.ComponentProps<typeof Separator>) => (
  <Separator className={cn(RESIZABLE_HANDLE_CLASS, className)} {...props} />
);

export {
  RESIZABLE_HANDLE_CLASS,
  ResizablePanelGroup,
  ResizablePanel,
  ResizableHandle,
};
