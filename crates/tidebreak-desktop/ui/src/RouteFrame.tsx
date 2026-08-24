import type { ReactNode } from "react";

/**
 * A route and the rail that belongs to it, side by side.
 *
 * The shell renders the window, the client and the outlet; the rail is chosen
 * here, by the route, because which controls make sense is exactly what the
 * route knows and the shell does not.
 */
export function RouteFrame({
  sidebar,
  children,
  className,
  mainClassName,
}: {
  sidebar: ReactNode;
  children: ReactNode;
  className?: string;
  mainClassName?: string;
}) {
  const frame = (
    <>
      {sidebar}
      <main className={`main${mainClassName ? ` ${mainClassName}` : ""}`}>
        {children}
      </main>
    </>
  );

  return className ? <div className={className}>{frame}</div> : frame;
}
