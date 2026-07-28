import { openExternal } from "./host";

/** Native opener first; `window.open` only works in a plain browser tab. */
export async function openSignInPage(url: string): Promise<void> {
  if (!(await openExternal(url).catch(() => false))) {
    window.open(url, "_blank", "noreferrer,noopener");
  }
}
