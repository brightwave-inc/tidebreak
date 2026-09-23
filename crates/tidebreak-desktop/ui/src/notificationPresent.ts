/**
 * How one agent notice is shown: a finished or failed turn, or an agent that
 * stopped for you. Never toast and native.
 */
export type NotificationPresentKind = "skip" | "toast" | "native" | "dock";

export type NotificationPermissionState =
  | "granted"
  | "denied"
  | "prompt"
  | "unavailable";

/**
 * Pick one presentation for a new agent notice.
 *
 * Window focus is the main window, not `document.hidden`. Looking at the
 * conversation in a focused window is signal enough, so it skips. Anywhere
 * else in a focused window gets a toast. A window in the background gets a
 * desktop banner.
 */
export function notificationPresent(input: {
  windowFocused: boolean;
  viewingConversation: boolean;
  permission: NotificationPermissionState;
  enabled?: boolean;
}): NotificationPresentKind {
  if (input.enabled === false) return "skip";
  if (input.windowFocused && input.viewingConversation) return "skip";
  if (input.windowFocused) return "toast";
  if (input.permission === "granted" || input.permission === "prompt") {
    return "native";
  }
  return "dock";
}

/** Whether this route is the conversation the row points at. */
export function viewingNotificationConversation(
  pathname: string,
  context:
    | { surface: "chat"; chatId: string }
    | {
        surface: "code";
        sessionId: string;
        workspaceId: string;
      },
): boolean {
  if (context.surface === "chat") {
    return (
      pathname === `/c/${context.chatId}` ||
      pathname.endsWith(`/c/${context.chatId}`)
    );
  }
  return (
    pathname === `/code/w/${context.workspaceId}` ||
    pathname.startsWith(`/code/w/${context.workspaceId}/`)
  );
}

/** Where opening a notification row takes you. */
export function notificationHref(row: {
  context:
    | { surface: "chat"; chatId: string }
    | { surface: "code"; workspaceId: string };
}): string {
  if (row.context.surface === "chat") {
    return `/c/${row.context.chatId}`;
  }
  return `/code/w/${row.context.workspaceId}`;
}
