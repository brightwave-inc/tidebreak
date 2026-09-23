import type { ReactNode } from "react";

/**
 * The corner where notices float over the page: the update card and the
 * notice after an unclean exit. They stack in one column instead of covering
 * each other, and the gaps between them stay clickable.
 */
export function FloatingNotices({ children }: { children: ReactNode }) {
  return (
    <div className="pointer-events-none fixed right-4 bottom-4 z-40 flex w-[min(22rem,calc(100vw-2rem))] flex-col gap-3 *:pointer-events-auto">
      {children}
    </div>
  );
}
