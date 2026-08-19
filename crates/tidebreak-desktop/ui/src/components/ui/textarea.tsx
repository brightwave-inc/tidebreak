import * as React from "react";
import { cn } from "@/lib/utils";

/**
 * Multi-line counterpart to `Input`, sharing its border, focus ring, and
 * disabled treatment. Height is left to the caller — `rows` or a `min-h-*`
 * override — because the fields that use one are sized to the content they
 * expect rather than to a single shared line height.
 */
const Textarea = React.forwardRef<
  HTMLTextAreaElement,
  React.ComponentProps<"textarea">
>(({ className, ...props }, ref) => {
  return (
    <textarea
      className={cn(
        "flex w-full rounded-lg border border-input bg-transparent px-2.5 py-2 text-base transition-colors outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/25 disabled:cursor-not-allowed disabled:bg-input/40 disabled:opacity-50 md:text-sm",
        className,
      )}
      ref={ref}
      {...props}
    />
  );
});
Textarea.displayName = "Textarea";

export { Textarea };
