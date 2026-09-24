import { ComputerUsePermissionDialog } from "./ComputerUsePermissionDialog";
import { ComputerUsePermissionNotice } from "./ComputerUsePermissionNotice";
import {
  isPermissionRequired,
  PERMISSION_REQUIRED_EVENT,
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
 * The notice a task leaves when it stopped for a missing permission, shown
 * while the ask is closed: after Not now, or when the ask did not open
 * because the person had chosen Not now before.
 */
export function ComputerUsePermissionNoticeHost({
  onOpenSettings,
}: {
  onOpenSettings: () => void;
}) {
  const ask = useComputerUsePermissionAsk((state) => state.ask);
  const need = useComputerUsePermissionAsk((state) => state.need);
  const openAsk = useComputerUsePermissionAsk((state) => state.openAsk);
  const dismissNeed = useComputerUsePermissionAsk((state) => state.dismissNeed);
  if (!need || ask) return null;
  return (
    <ComputerUsePermissionNotice
      need={need}
      onAllow={() => openAsk(need.browser ? "browser" : "apps")}
      onOpenSettings={() => {
        dismissNeed();
        onOpenSettings();
      }}
      onDismiss={dismissNeed}
    />
  );
}
