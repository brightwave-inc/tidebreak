import { useEffect } from "react";
import { useRouterState } from "@tanstack/react-router";

import { useChatListStore } from "./ChatListStore";
import { useCodeCatalogStore } from "./code/CodeCatalogStore";
import { formatDocumentTitle, pageNameForPath } from "./pageTitle";
import { useProjectListStore } from "./ProjectListStore";
import { SETTINGS_SECTIONS } from "./settings/sections";
import { useActiveChatId } from "./useActiveChatId";

/**
 * Keeps `document.title` in step with the open route so a screen reader and
 * the window chrome name the page the reader is on.
 */
export function DocumentTitle() {
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const chatId = useActiveChatId();
  const conversation = useChatListStore((state) =>
    chatId
      ? state.chats.find((chat) => chat.id === chatId)?.title?.trim()
      : undefined,
  );
  const workspace = useCodeCatalogStore((state) => {
    const match = /^\/code\/w\/([^/]+)$/.exec(pathname);
    if (!match) return undefined;
    return state.workspaces.find((item) => item.id === match[1])?.title?.trim();
  });
  const project = useProjectListStore((state) => {
    const match = /^\/p\/([^/]+)$/.exec(pathname);
    if (!match) return undefined;
    return state.projects.find((item) => item.id === match[1])?.title?.trim();
  });
  const settings = SETTINGS_SECTIONS.find(
    (section) => pathname === `/settings/${section.path}`,
  )?.label;

  useEffect(() => {
    document.title = formatDocumentTitle(
      pageNameForPath(pathname, {
        conversation,
        workspace,
        project,
        settings,
      }),
    );
  }, [conversation, pathname, project, settings, workspace]);

  return null;
}
