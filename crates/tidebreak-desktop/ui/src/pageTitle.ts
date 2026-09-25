/** Browser tab title: "<page or conversation name> – Tidebreak". */
export const DOCUMENT_TITLE_PRODUCT = "Tidebreak";

export type DocumentTitleNames = {
  conversation?: string;
  workspace?: string;
  project?: string;
  settings?: string;
  session?: string;
  app?: string;
  plugin?: string;
};

export function formatDocumentTitle(page: string): string {
  const name = page.trim();
  if (!name || name === DOCUMENT_TITLE_PRODUCT) return DOCUMENT_TITLE_PRODUCT;
  return `${name} – ${DOCUMENT_TITLE_PRODUCT}`;
}

export function pageNameForPath(
  pathname: string,
  names: DocumentTitleNames = {},
): string {
  if (pathname === "/" || pathname === "") return "Home";
  if (pathname === "/inbox") return "Inbox";
  if (pathname === "/apps") return "Apps";
  if (pathname.startsWith("/apps/")) return names.app ?? "Apps";
  if (pathname === "/plugins") return "Plugins";
  if (pathname.startsWith("/plugins/")) return names.plugin ?? "Plugins";
  if (pathname.startsWith("/settings")) return names.settings ?? "Settings";
  if (/^\/p\/[^/]+\/c\//.test(pathname) || pathname.startsWith("/c/")) {
    return names.conversation ?? "Conversation";
  }
  if (/^\/p\/[^/]+$/.test(pathname)) return names.project ?? "Project";
  if (pathname === "/code") return "Code";
  if (pathname === "/code/inbox") return "Inbox";
  if (pathname === "/code/analytics") return "Analytics";
  if (pathname === "/code/archive") return "Archive";
  if (pathname === "/code/delivery/pull-requests") return "Pull requests";
  if (pathname === "/code/delivery/runs") return "Runs & deployments";
  if (pathname.startsWith("/code/w/")) return names.workspace ?? "Workspace";
  if (pathname.startsWith("/code/s/")) return names.session ?? "Conversation";
  return DOCUMENT_TITLE_PRODUCT;
}
