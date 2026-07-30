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
        "flex w-full rounded-md border border-border bg-background px-3 py-2 text-base ring-offset-background placeholder:text-muted-foreground focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50 md:text-sm",
        className,
      )}
      ref={ref}
      {...props}
    />
  );
});
Textarea.displayName = "Textarea";

export { Textarea };
