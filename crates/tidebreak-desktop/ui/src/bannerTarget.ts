import { useEffect, useRef } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";

/**
 * Where the last desktop banner points, so opening the app from it opens the
 * conversation it was about.
 *
 * The notification plugin reports no click on the desktop. Clicking a banner
 * activates the app, so the window's next focus stands in for the click. Two
 * checks keep a later, unrelated return from moving you: the target expires,
 * and it only counts while its conversation still wants you.
 */

/** How long after a banner the app's activation still counts as its click. */
export const BANNER_TARGET_TTL_MS = 10 * 60_000;

type BannerTarget = {
  href: string;
  postedAt: number;
  stillWanted: () => boolean;
};

let pending: BannerTarget | null = null;

/**
 * Remember the banner just posted. Only the newest counts: macOS says which
 * app was activated, not which of its banners was clicked.
 */
export function rememberBannerTarget(
  href: string,
  stillWanted: () => boolean = () => true,
  now: number = Date.now(),
): void {
  pending = { href, postedAt: now, stillWanted };
}

/** The route to open for this activation, once, or `null` when none is due. */
export function takeBannerTarget(now: number = Date.now()): string | null {
  const target = pending;
  pending = null;
  if (!target) return null;
  if (now - target.postedAt > BANNER_TARGET_TTL_MS) return null;
  if (!target.stillWanted()) return null;
  return target.href;
}

/** Drop any remembered target. For a client switch and for tests. */
export function forgetBannerTarget(): void {
  pending = null;
}

/**
 * Open the last banner's conversation when the window comes back to the
 * front. Mounted once, in the shell.
 */
export function useBannerTargetNavigation(): void {
  const navigate = useNavigate();
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const pathnameRef = useRef(pathname);
  pathnameRef.current = pathname;

  useEffect(() => {
    const onFocus = () => {
      const href = takeBannerTarget();
      if (!href || href === pathnameRef.current) return;
      void navigate({ to: href });
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [navigate]);
}
