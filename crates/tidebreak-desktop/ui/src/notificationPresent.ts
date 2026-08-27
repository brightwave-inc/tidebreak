/** How one unread agent-finished row is shown. Never toast and native. */
export type NotificationPresentKind = "skip" | "toast" | "native" | "dock";

export type NotificationPermissionState =
  | "granted"
  | "denied"
  | "prompt"
  | "unavailable";

/**
 * Pick one presentation for a newly unread agent-finished row.
 *
 * Window focus is the main window, not `document.hidden`. Viewing the
 * conversation itself is enough signal; anything else toast or native.
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
