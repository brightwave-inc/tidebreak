/**
 * Interactive notification categories and Android channels (mg ADR 0093).
 *
 * Three of the gateway's notified kinds are moments the owner may want to act
 * on from the tray without opening the app:
 *
 *   possibly_stalled    → Nudge (status check) / Cancel run
 *   soft_ceiling_warned → Cancel now (default: let it wrap up)
 *   acceptance_met      → Accept & stop (default: keep going)
 *
 * `task_complete` and `daily_budget` stay tap-only: there is no decision in
 * them. Category identifiers are the kinds themselves — the gateway stamps
 * `categoryId: kind` on the message, and iOS only attaches actions to a remote
 * notification whose payload names a category registered here. A kind with no
 * registered category renders tap-only, unchanged.
 *
 * Registration is plain runtime JS re-run on every start, so a change to it
 * ships over the air with no fingerprint impact.
 */

import * as Notifications from "expo-notifications";
import { Platform } from "react-native";

/**
 * Android channel id. A fresh id rather than "default" because Android locks
 * a channel's importance at creation and cannot raise it later.
 */
export const ANDROID_CHANNEL_ID = "tidebreak-alerts";

/**
 * Low-importance sibling channel for in-place updates to a notification the
 * user just acted on. Separate because expo-notifications exposes no
 * `onlyAlertOnce`: a same-channel re-post buzzes again, and an update to
 * something just pressed must be silent.
 */
export const ANDROID_UPDATES_CHANNEL_ID = "tidebreak-updates";

/** The kinds whose notifications carry decision actions. */
export const DECISION_KINDS = [
  "possibly_stalled",
  "soft_ceiling_warned",
  "acceptance_met",
] as const;

/**
 * Action identifiers, globally unique across categories so a handler can
 * dispatch on `actionIdentifier` alone — the content-minimized payload carries
 * no kind, and Android does not echo the category back.
 */
export const NUDGE_ACTION = "sandbox_nudge";
export const CANCEL_ACTION = "sandbox_cancel";
export const ACCEPT_STOP_ACTION = "sandbox_accept_stop";

export type DecisionAction = "cancel" | "accept_stop" | "nudge";

/** Which decision an action identifier names, or null when it names none. */
export function decisionAction(
  actionIdentifier: string,
): DecisionAction | null {
  switch (actionIdentifier) {
    case CANCEL_ACTION:
      return "cancel";
    case ACCEPT_STOP_ACTION:
      return "accept_stop";
    case NUDGE_ACTION:
      return "nudge";
    default:
      return null;
  }
}

/**
 * The fixed Nudge body — neutral, and read only by the harness. Sent as an
 * interrupt: a possibly-stalled run is suspected wedged mid-turn, and a
 * non-interrupt message would wait for a turn boundary that may never come.
 */
export const NUDGE_MESSAGE_BODY =
  "Status check from your owner: summarize your current progress and what " +
  "you will do next. If you are blocked, say what is blocking you. Then " +
  "continue the task.";

let channelsEnsured: Promise<void> | null = null;

/**
 * Creates both Android channels, memoized per process. Every path that
 * presents a notification must be able to call this: the headless entry points
 * mount no React tree, and Android 8+ silently drops a notify into a channel
 * that does not exist yet.
 */
export function ensureAndroidChannels(): Promise<void> {
  if (Platform.OS !== "android") {
    return Promise.resolve();
  }
  channelsEnsured ??= (async () => {
    try {
      // Deliberately no `sound` key: expo-notifications resolves a string
      // there as a bundled raw resource and fails. The message-level "default"
      // sound is what actually plays.
      await Notifications.setNotificationChannelAsync(ANDROID_CHANNEL_ID, {
        name: "Tidebreak alerts",
        importance: Notifications.AndroidImportance.HIGH,
      });
      await Notifications.setNotificationChannelAsync(
        ANDROID_UPDATES_CHANNEL_ID,
        {
          name: "Tidebreak updates",
          importance: Notifications.AndroidImportance.LOW,
        },
      );
    } catch {
      // Retried at the next call site.
      channelsEnsured = null;
    }
  })();
  return channelsEnsured;
}

/** Never open the app; the action completes (or fails visibly) in the tray. */
const BACKGROUND = { opensAppToForeground: false };

/**
 * Registers the decision categories. Safe on every start — expo-notifications
 * overwrites in place — and failures are swallowed: a device without
 * categories degrades to tap-only notifications, which is the pre-existing
 * behaviour rather than a broken one.
 */
export async function ensureNotificationCategories(): Promise<void> {
  try {
    await Promise.all([
      Notifications.setNotificationCategoryAsync("possibly_stalled", [
        { identifier: NUDGE_ACTION, buttonTitle: "Nudge", options: BACKGROUND },
        {
          identifier: CANCEL_ACTION,
          buttonTitle: "Cancel run",
          options: { ...BACKGROUND, isDestructive: true },
        },
      ]),
      // One action, and deliberately no "keep going" button: letting the run
      // use its wrap-up window is the default, and the default is inaction.
      Notifications.setNotificationCategoryAsync("soft_ceiling_warned", [
        {
          identifier: CANCEL_ACTION,
          buttonTitle: "Cancel now",
          options: { ...BACKGROUND, isDestructive: true },
        },
      ]),
      // Not styled destructive: stopping a run that met its acceptance
      // criteria is the success path, and its output is kept.
      Notifications.setNotificationCategoryAsync("acceptance_met", [
        {
          identifier: ACCEPT_STOP_ACTION,
          buttonTitle: "Accept & stop",
          options: BACKGROUND,
        },
      ]),
    ]);
  } catch {
    // Retried on the next mount.
  }
}
