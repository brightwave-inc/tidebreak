import type { ShellShortcutMode } from "@/ShellShortcuts";

/**
 * Which mode a path is in, decided by the route family alone.
 *
 * Code mode is a place in the URL, not a flag someone can leave set: the rail,
 * the shortcuts, and anything else that behaves differently on the two sides of
 * the app read it from here so they cannot come to disagree. Kept free of
 * imports beyond a type so the shell can ask without pulling code mode in.
 */

/** Whether a path belongs to code mode. */
export function isCodeRoute(pathname: string): boolean {
  return pathname === "/code" || pathname.startsWith("/code/");
}

/** The shortcut scope a path puts the reader in. */
export function shellShortcutMode(pathname: string): ShellShortcutMode {
  return isCodeRoute(pathname) ? "code" : "chat";
}

/**
 * The repo a path is showing, when it is showing one.
 *
 * Lets a shortcut fired on a repo page open the same pre-selected repo the
 * page's own button does. The reader can still pick a different one.
 */
export function codeRepoIdFromPath(pathname: string): string | undefined {
  const match = /^\/code\/r\/([^/]+)$/.exec(pathname);
  return match?.[1];
}

/**
 * The workspace a path is showing, when it is showing one.
 *
 * The terminal drawer and review-sidebar shortcuts only mean something on a
 * workspace, so the shell reads the id from here the same way Cmd+N reads
 * the repo.
 */
export function codeWorkspaceIdFromPath(pathname: string): string | undefined {
  const match = /^\/code\/w\/([^/]+)$/.exec(pathname);
  return match?.[1];
}
