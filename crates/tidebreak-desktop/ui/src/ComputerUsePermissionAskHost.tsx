import { useChatListStore } from "./ChatListStore";
import {
  useCodeUpdatesStore,
  type CodeUpdatesState,
} from "./code/CodeUpdatesStore";
import { ComputerUsePermissionDialog } from "./ComputerUsePermissionDialog";
import { ComputerUsePermissionNotice } from "./ComputerUsePermissionNotice";
import {
  isPermissionRequired,
  PERMISSION_REQUIRED_EVENT,
  type PermissionRequired,
  useComputerUsePermissionAsk,
} from "./computerUsePermissionAsk";
import {
  computerUsePermissionHost,
  type ComputerUsePermissionHost,
} from "./computerUsePermissions";
import { useNativeHostEvent } from "./nativeMenu";

/**
 * Listens for tasks that need a macOS permission and opens the ask when one
 * should open.
 *
 * Nothing here runs at launch: no permission read, no dialog. The dialog
 * mounts only while it is asking, because it reads permissions and watches
 * window focus for as long as it is mounted.
 */
export function ComputerUsePermissionAskHost({
  host = computerUsePermissionHost,
  onOpenSettings,
}: {
  host?: ComputerUsePermissionHost;
  onOpenSettings: () => void;
}) {
  const ask = useComputerUsePermissionAsk((state) => state.ask);
  const taskNeedsPermission = useComputerUsePermissionAsk(
    (state) => state.taskNeedsPermission,
  );
  const closeAsk = useComputerUsePermissionAsk((state) => state.closeAsk);

  useNativeHostEvent(PERMISSION_REQUIRED_EVENT, (payload) => {
    // Only this Mac's own tasks report here, and only its window can grant.
    if (host.availability() !== "local" || !isPermissionRequired(payload)) {
      return;
    }
    taskNeedsPermission(payload);
  });

  if (!ask) return null;
  return (
    <ComputerUsePermissionDialog
      ask={ask}
      host={host}
      onClose={closeAsk}
      onOpenSettings={() => {
        closeAsk("not_now");
        onOpenSettings();
      }}
    />
  );
}

/**
 * The notices tasks leave when they stopped for a missing permission, one per
 * task, shown while the ask is closed: after Not now, or when the ask did not
 * open because the person had chosen Not now before.
 */
export function ComputerUsePermissionNoticeHost({
  onOpenSettings,
}: {
  onOpenSettings: () => void;
}) {
  const ask = useComputerUsePermissionAsk((state) => state.ask);
  const needs = useComputerUsePermissionAsk((state) => state.needs);
  if (ask) return null;
  return needs.map((need) => (
    <TaskPermissionNotice
      key={need.taskId}
      need={need}
      onOpenSettings={onOpenSettings}
    />
  ));
}

function TaskPermissionNotice({
  need,
  onOpenSettings,
}: {
  need: PermissionRequired;
  onOpenSettings: () => void;
}) {
  const openAsk = useComputerUsePermissionAsk((state) => state.openAsk);
  const dismissNeed = useComputerUsePermissionAsk((state) => state.dismissNeed);
  const taskName = useTaskName(need.taskId);
  return (
    <ComputerUsePermissionNotice
      need={need}
      taskName={taskName}
      onAllow={() => openAsk(need.browser ? "browser" : "apps")}
      onOpenSettings={() => {
        dismissNeed(need.taskId);
        onOpenSettings();
      }}
      onDismiss={() => dismissNeed(need.taskId)}
    />
  );
}

/**
 * The title a task goes by in the window: its conversation's, or its code
 * session's. `null` when neither list has it loaded.
 */
function useTaskName(taskId: string): string | null {
  const chatTitle = useChatListStore(
    (state) => state.chats.find((chat) => chat.id === taskId)?.title ?? null,
  );
  const sessionTitle = useCodeUpdatesStore((state) =>
    codeSessionTitle(state, taskId),
  );
  return chatTitle || sessionTitle || null;
}

function codeSessionTitle(
  state: CodeUpdatesState,
  sessionId: string,
): string | null {
  const digest =
    state.conversationsWithoutWorkspace[sessionId] ??
    [state.conversationsByWorkspace, state.childrenByWorkspace]
      .flatMap((byWorkspace) => Object.values(byWorkspace))
      .map((sessions) => sessions[sessionId])
      .find(Boolean);
  return digest?.title ?? null;
}
