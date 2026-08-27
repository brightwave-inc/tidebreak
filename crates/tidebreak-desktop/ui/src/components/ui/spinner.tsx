import * as React from "react";
import { cn } from "@/lib/utils";

/**
 * The indeterminate-progress glyph, in one place.
 *
 * Size and colour are ordinary class overrides: `size-3` for a badge, an
 * explicit `text-*` where the spinner should take its surroundings' colour
 * instead of the muted default. Callers own the accessible name — pass
 * `aria-hidden` when adjacent text already says what is happening, or an
 * `aria-label` when the glyph is the only signal.
 *
 * The arc is a circle centred on the 24 viewBox. Lucide's Loader2 writes
 * width/height="24" and uses an open path, so WebKit's `animate-spin`
 * origin falls on the C's ink box and the glyph orbits at badge size.
 * `transform-box: view-box` pins rotation to the viewBox centre.
 */
const Spinner = React.forwardRef<SVGSVGElement, React.SVGProps<SVGSVGElement>>(
  ({ className, ...props }, ref) => {
    return (
      <svg
        ref={ref}
        viewBox="0 0 24 24"
        fill="none"
        xmlns="http://www.w3.org/2000/svg"
        // An `svg` is not an element that takes a name, so a caller's
        // `aria-label` would be dropped without a role that does.
        role={props["aria-label"] ? "img" : undefined}
        {...props}
        className={cn(
          "inline-block size-4 shrink-0 origin-center animate-spin text-muted-foreground [transform-box:view-box]",
          className,
        )}
      >
        <circle
          cx="12"
          cy="12"
          r="9"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeDasharray="42 14.5"
        />
      </svg>
    );
  },
);
Spinner.displayName = "Spinner";

export { Spinner };
