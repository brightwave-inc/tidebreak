/**
 * Background renderer for Android decision pushes.
 *
 * A backgrounded Expo-service push carrying a notification payload is rendered
 * by the Firebase SDK before any app code runs, so notification-category
 * action buttons never attach on Android. For the decision kinds, a gateway
 * that knows this build can render (the `renders_data_messages` registration
 * flag) sends a data-only message instead: nothing displays on its own, this
 * task wakes — foreground, background, or killed — and renders a local
 * notification with the category attached. iOS never receives the data-only
 * form; its buttons come from the OS-native category match on the display
 * form.
 *
 * Defined at module scope and imported from the app entry (`index.js`), as
 * headless launches require: on a killed-app delivery only the entry chain's
 * module scope has run, and a task defined inside a React tree would not
 * exist yet.
 *
 * Note that this repository ships no `google-services.json`, so an Android
 * build has no FCM registration, never claims `renders_data_messages`, and
 * therefore never receives the data-only form. The task arms and stays silent
 * — which is the intended no-op until the Firebase config lands.
 */

import * as Notifications from "expo-notifications";
import * as TaskManager from "expo-task-manager";
import { Platform } from "react-native";
import { executeDecisionResponse } from "./decisionResponses";
import {
  ANDROID_CHANNEL_ID,
  DECISION_KINDS,
  ensureAndroidChannels,
} from "./categories";

export const NOTIFICATION_RENDER_TASK = "tidebreak-notification-render";

/**
 * The gateway's data-only payload. The native side surfaces the Expo message's
 * `data` object JSON-encoded under `dataString`; the gateway copies the
 * rendered `title`/`body` into it precisely so this renderer needs no other
 * source.
 */
function parsePayload(taskData: unknown): Record<string, unknown> | null {
  if (!taskData || typeof taskData !== "object") {
    return null;
  }
  const data = (taskData as { data?: { dataString?: unknown } }).data;
  if (!data || typeof data.dataString !== "string") {
    return null;
  }
  try {
    const parsed: unknown = JSON.parse(data.dataString);
    return parsed && typeof parsed === "object"
      ? (parsed as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

function str(payload: Record<string, unknown>, key: string): string | undefined {
  const value = payload[key];
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

/**
 * An action-button press while the app is backgrounded or killed also arrives
 * through this task rather than through the UI listeners, which a killed app
 * never set up. The serialized shape is the response bundle:
 * `actionIdentifier` at the top, the pressed notification underneath, its data
 * either as an object (a local render) or a JSON `dataString` (remote
 * content).
 */
function parseResponse(taskData: unknown): {
  actionIdentifier: string;
  requestIdentifier: string;
  data: Record<string, unknown>;
} | null {
  if (!taskData || typeof taskData !== "object") {
    return null;
  }
  const response = taskData as {
    actionIdentifier?: unknown;
    notification?: {
      request?: {
        identifier?: unknown;
        content?: { data?: unknown; dataString?: unknown };
      };
    };
  };
  if (typeof response.actionIdentifier !== "string") {
    return null;
  }
  const request = response.notification?.request;
  if (!request || typeof request.identifier !== "string") {
    return null;
  }
  let data = request.content?.data;
  if (!data && typeof request.content?.dataString === "string") {
    try {
      data = JSON.parse(request.content.dataString) as unknown;
    } catch {
      data = undefined;
    }
  }
  if (!data || typeof data !== "object") {
    return null;
  }
  return {
    actionIdentifier: response.actionIdentifier,
    requestIdentifier: request.identifier,
    data: data as Record<string, unknown>,
  };
}

TaskManager.defineTask(NOTIFICATION_RENDER_TASK, async ({ data, error }) => {
  if (error || Platform.OS !== "android") {
    return;
  }
  const response = parseResponse(data);
  if (response) {
    // A button press, not an incoming message. The executor hydrates the
    // registry itself and dedupes against the replay Android queues for the
    // next app open.
    await executeDecisionResponse(response);
    return;
  }
  const payload = parsePayload(data);
  if (!payload) {
    return;
  }
  const kind = str(payload, "kind");
  const title = str(payload, "title");
  if (!kind || !title || !(DECISION_KINDS as readonly string[]).includes(kind)) {
    // Not a payload this renderer owns. Display-form messages never reach here
    // — the Firebase SDK renders those before the app runs — so there is
    // nothing to fall back to, and an unowned data message stays silent by
    // design rather than rendering half a notification.
    return;
  }
  try {
    // A killed-app delivery mounts no React tree, so nothing else guarantees
    // the channel exists, and Android 8+ drops a notify into a missing one.
    await ensureAndroidChannels();
    await Notifications.scheduleNotificationAsync({
      // The collapse key as the identifier keeps tray coalescing: a repeat
      // alert for the same run replaces its predecessor, as the display form's
      // tag did.
      identifier: str(payload, "collapse"),
      content: {
        title,
        body: str(payload, "body") ?? "",
        categoryIdentifier: kind,
        // The whole payload rides along so a press reads sandbox_id and
        // installation_id exactly as it would from a remote notification.
        data: payload,
      },
      trigger: { channelId: ANDROID_CHANNEL_ID },
    });
  } catch {
    // A render failure must never crash a headless launch.
  }
});

/**
 * Arms the task with expo-notifications. Idempotent, and safe on iOS, where it
 * simply never fires.
 */
export async function ensureNotificationRenderTask(): Promise<void> {
  try {
    await Notifications.registerTaskAsync(NOTIFICATION_RENDER_TASK);
  } catch {
    // Retried on the next mount.
  }
}
