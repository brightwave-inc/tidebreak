import * as React from "react";
import * as PopoverPrimitive from "@radix-ui/react-popover";

import { cn } from "@/lib/utils";

const Popover = PopoverPrimitive.Root;
const PopoverTrigger = PopoverPrimitive.Trigger;
const PopoverPortal = PopoverPrimitive.Portal;
const PopoverClose = PopoverPrimitive.Close;
const PopoverAnchor = PopoverPrimitive.Anchor;

const PopoverCollisionBoundaryContext = React.createContext<HTMLElement | null>(null);

/**
 * Keeps popovers inside their panel. A popover opened near the edge of a narrow
 * panel would otherwise flip out over the neighbouring panel, which reads as a
 * popover belonging to the wrong content.
 */
function PopoverCollisionBoundaryProvider({
  boundary,
  children,
}: React.PropsWithChildren<{ boundary: HTMLElement | null }>) {
  return (
    <PopoverCollisionBoundaryContext.Provider value={boundary}>
      {children}
    </PopoverCollisionBoundaryContext.Provider>
  );
}

const PopoverContent = React.forwardRef<
  React.ComponentRef<typeof PopoverPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof PopoverPrimitive.Content>
>(({ className, align = "center", sideOffset = 4, collisionBoundary, ...props }, ref) => {
  const contextualBoundary = React.useContext(PopoverCollisionBoundaryContext);
  return (
    <PopoverPrimitive.Portal>
      <PopoverPrimitive.Content
        ref={ref}
        align={align}
        sideOffset={sideOffset}
        collisionBoundary={collisionBoundary ?? contextualBoundary ?? undefined}
        className={cn(
          "z-50 w-72 rounded-md border bg-popover p-4 text-popover-foreground shadow-lg outline-hidden data-[side=bottom]:slide-in-from-top-2 data-[side=left]:slide-in-from-right-2 data-[side=right]:slide-in-from-left-2 data-[side=top]:slide-in-from-bottom-2 data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-95 data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-95",
          className,
        )}
        {...props}
      />
    </PopoverPrimitive.Portal>
  );
});
PopoverContent.displayName = PopoverPrimitive.Content.displayName;

export {
  Popover,
  PopoverTrigger,
  PopoverContent,
  PopoverPortal,
  PopoverClose,
  PopoverAnchor,
  PopoverCollisionBoundaryProvider,
};
