import type { ComponentPropsWithoutRef } from "react";

import { cn } from "@/lib/utils";

type LiveLabelProps = {
  children: string;
  /** Sweep a highlight across the glyphs while work is happening. */
  live?: boolean;
} & Omit<ComponentPropsWithoutRef<"span">, "children">;

/**
 * Status copy that can shimmer. The highlight is a second copy of the
 * string in `data-text`, so keep `children` a plain string.
 */
export function LiveLabel({
  children,
  live = false,
  className,
  ...props
}: LiveLabelProps) {
  return (
    <span
      {...props}
      className={cn(live && "live-label-shimmer", className)}
      data-text={live ? children : undefined}
    >
      {children}
    </span>
  );
}
