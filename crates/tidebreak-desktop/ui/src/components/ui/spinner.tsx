import * as React from "react";
import { Loader2 } from "lucide-react";
import { cn } from "@/lib/utils";

/**
 * The indeterminate-progress glyph, in one place.
 *
 * Size and colour are ordinary class overrides: `size-3` for a badge, an
 * explicit `text-*` where the spinner should take its surroundings' colour
 * instead of the muted default. Callers own the accessible name — pass
 * `aria-hidden` when adjacent text already says what is happening, or an
 * `aria-label` when the glyph is the only signal.
 */
const Spinner = React.forwardRef<
  SVGSVGElement,
  React.ComponentProps<typeof Loader2>
>(({ className, ...props }, ref) => {
  return (
    <Loader2
      className={cn("size-4 animate-spin text-muted-foreground", className)}
      ref={ref}
      {...props}
    />
  );
});
Spinner.displayName = "Spinner";

export { Spinner };
