import { openExternal } from "./host";

/**
 * Open a URL in the user's own browser.
 *
 * Native opener first; `window.open` only works in a plain browser tab. Used
 * for everything that has to leave the app — a gateway or provider sign-in,
 * and the gateway page where a shared app is published.
 */
export async function openInBrowser(url: string): Promise<void> {
  if (!(await openExternal(url).catch(() => false))) {
    window.open(url, "_blank", "noreferrer,noopener");
  }
}
