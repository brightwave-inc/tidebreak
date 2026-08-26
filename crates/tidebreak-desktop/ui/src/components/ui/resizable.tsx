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

const ResizableHandle = ({
  className,
  ...props
}: React.ComponentProps<typeof Separator>) => (
  <Separator
    className={cn(
      "relative flex w-px items-center justify-center bg-border after:absolute after:inset-y-0 after:left-1/2 after:w-1 after:-translate-x-1/2 focus-visible:ring-1 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:outline-hidden aria-[orientation=vertical]:h-px aria-[orientation=vertical]:w-full aria-[orientation=vertical]:after:left-0 aria-[orientation=vertical]:after:h-1 aria-[orientation=vertical]:after:w-full aria-[orientation=vertical]:after:translate-x-0 aria-[orientation=vertical]:after:-translate-y-1/2",
      className,
    )}
    {...props}
  />
);

export { ResizablePanelGroup, ResizablePanel, ResizableHandle };
