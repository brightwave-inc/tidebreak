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
}: {
  sidebar: ReactNode;
  children: ReactNode;
}) {
  return (
    <>
      {sidebar}
      <div className="main">{children}</div>
    </>
  );
}
