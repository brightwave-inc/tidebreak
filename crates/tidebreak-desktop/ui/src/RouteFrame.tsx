import { useLayoutEffect, useRef, type ReactNode } from "react";
import { useRouter } from "@tanstack/react-router";

/** Last path that owned the page heading, so a remounted frame still skips the first load. */
let lastFocusedPath: string | null = null;

/**
 * Whether a route change may move focus to the page heading.
 *
 * Focus stays put while the person is typing or a dialog owns it, so a
 * navigation that happens under them (sending a first message opens its chat)
 * does not pull the caret away.
 */
export function headingMayTakeFocus(active: Element | null): boolean {
  if (!(active instanceof HTMLElement) || active === document.body) return true;
  if (active.isContentEditable) return false;
  return !active.closest(
    "input, textarea, select, [contenteditable]:not([contenteditable='false']), dialog, [role='dialog'], [role='alertdialog']",
  );
}

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
  const mainRef = useRef<HTMLElement>(null);
  const router = useRouter();
  useLayoutEffect(() => {
    const apply = () => {
      const pathname = router.state.location.pathname;
      const heading = mainRef.current?.querySelector("h1");
      if (!(heading instanceof HTMLElement)) return;
      heading.tabIndex = -1;
      const previous = lastFocusedPath;
      lastFocusedPath = pathname;
      if (previous === null || previous === pathname) return;
      if (!headingMayTakeFocus(document.activeElement)) return;
      heading.focus({ preventScroll: true });
    };
    apply();
    return router.subscribe("onResolved", apply);
  }, [router]);

  const frame = (
    <>
      {sidebar}
      <main
        ref={mainRef}
        className={`main${mainClassName ? ` ${mainClassName}` : ""}`}
      >
        {children}
      </main>
    </>
  );

  return className ? <div className={className}>{frame}</div> : frame;
}
