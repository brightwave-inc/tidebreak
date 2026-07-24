import { useState, type HTMLAttributes, type ReactNode } from "react";
import { cn } from "@/lib/utils";

/**
 * A height-capped scroll region that fades its bottom edge only while there is
 * more to read.
 *
 * An unconditional fade dims the last line of content that already fits, which
 * reads as a rendering fault rather than as an invitation to scroll.
 */
export function ScrollableContainer({
  className,
  children,
  ...props
}: HTMLAttributes<HTMLPreElement> & { children: ReactNode }) {
  const [overflowing, setOverflowing] = useState(false);
  const [atBottom, setAtBottom] = useState(false);

  const measure = (element: HTMLPreElement | null) => {
    if (!element) return;
    setOverflowing(element.scrollHeight > element.clientHeight);
  };

  return (
    <pre
      ref={measure}
      onScroll={(event) => {
        const element = event.currentTarget;
        setAtBottom(
          element.scrollTop + element.clientHeight >= element.scrollHeight,
        );
      }}
      className={cn(
        "max-h-40 overflow-auto",
        overflowing &&
          !atBottom &&
          "[mask-image:linear-gradient(180deg,#000_85%,transparent)]",
        className,
      )}
      {...props}
    >
      {children}
    </pre>
  );
}
