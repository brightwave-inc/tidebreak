/**
 * Executing a notification action button (mg ADR 0093).
 *
 * Two delivery paths reach here: the UI response listener (a foregrounded app,
 * and every iOS press — the OS wakes the app into it) and the background task
 * (Android presses while backgrounded or killed, which expo-notifications
 * hands to the task runner instead of the listeners).
 *
 * Runs headless-safely. A killed-app press launches the bundle with no React
 * tree, so the executor hydrates the connection registry itself rather than
 * assuming a screen did.
 *
 * Dedupe is persistent rather than per-process (`claimNotificationResponse`),
 * because Android queues a killed-app response and re-delivers it to the
 * listeners on the next app open: one press can legitimately reach JS twice,
 * in two different processes.
 *
 * Authority is the one place this deliberately diverges from doing the work.
 * The owner verbs live on the `runtime:<slug>` resource, which a session only
 * holds when its grant carries `runtime:execute` — a pairing made against a
 * gateway that has not been widened does not. Rather than firing a request
 * that will be refused, the press degrades to a tray message pointing at the
 * app. Administrator verbs are not offered at all: a notification is written
 * for the run's owner, and the owner cancel is the right (and only) verb.
 *
 * The slug itself is never a reason to refuse: `runtimeSlug.ts` records why a
 * fallback always mints a working token, so discovery failing here costs the
 * audit trail a meaningful audience and nothing else.
 */

import * as Notifications from "expo-notifications";
import { Platform } from "react-native";
import type { AppListResponse } from "../lib/consoleTypes";
import { GatewayClient, hasErrorCode } from "../lib/gatewayClient";
import { claimNotificationResponse } from "../lib/push";
import { grantsRuntimeExecute } from "../lib/scope";
import { RESOURCE_CONTROL, runtimeResource } from "../lib/resource";
import { runtimeSlugFrom } from "../lib/runtimeSlug";
import {
  connections,
  hydrateConnections,
  secureStorage,
} from "../session/runtime";
import type { GatewayConnection } from "../lib/connections";
import {
  ANDROID_UPDATES_CHANNEL_ID,
  decisionAction,
  ensureAndroidChannels,
  NUDGE_MESSAGE_BODY,
  type DecisionAction,
} from "./categories";

/** Everything the executor needs from either delivery path. */
export type DecisionResponse = {
  actionIdentifier: string;
  /** The pressed notification's identifier, for dedupe and in-place updates. */
  requestIdentifier: string;
  /** The notification's data payload. */
  data: Record<string, unknown>;
};

function pickString(
  data: Record<string, unknown>,
  key: string,
): string | null {
  const value = data[key];
  return typeof value === "string" && value.length > 0 ? value : null;
}

/**
 * The tray is the only feedback channel for a background action, so say there
 * what happened. Local-only — this never transits Expo — so unlike the push
 * payload it may name the run in a human sentence.
 *
 * On Android the pressed notification is still standing, so feedback morphs it
 * in place: a re-post under the same identifier replaces it, with no category,
 * so the buttons drop away with the decision. The updates channel is
 * low-importance so the morph never re-buzzes. On iOS the OS already dismissed
 * what was pressed, so only the final outcome is worth presenting.
 */
async function presentFeedback(
  response: DecisionResponse,
  title: string,
  body: string,
  updateOnly: boolean,
): Promise<void> {
  if (Platform.OS !== "android" && updateOnly) {
    return;
  }
  try {
    await ensureAndroidChannels();
    await Notifications.scheduleNotificationAsync(
      Platform.OS === "android"
        ? {
            identifier: response.requestIdentifier,
            content: { title, body, data: response.data },
            trigger: { channelId: ANDROID_UPDATES_CHANNEL_ID },
          }
        : { content: { title, body }, trigger: null },
    );
  } catch {
    // Presenting feedback must never throw into the handler.
  }
}

function pendingCopy(
  action: DecisionAction,
  runName: string,
): [string, string] {
  switch (action) {
    case "cancel":
      return ["Cancelling…", `Asking the gateway to stop ${runName}.`];
    case "accept_stop":
      return ["Stopping…", `Accepting ${runName}'s result.`];
    case "nudge":
      return ["Sending nudge…", `Interrupting ${runName} for a status update.`];
  }
}

/** The connection a push names, or null when this phone does not hold it. */
function connectionFor(installationId: string | null): GatewayConnection | null {
  const list = connections.list();
  if (!installationId) {
    return connections.active();
  }
  return (
    list.find((entry) => entry.installationId === installationId) ?? null
  );
}

/**
 * A client for one resource of a *named* connection.
 *
 * Deliberately not `consoleClients.ts`: those factories resolve whichever
 * connection is active, and a push can name a gateway that is not it. Minting
 * against the connection the payload names — rather than switching the active
 * one to match — keeps a tray press from silently re-pointing the whole app.
 */
function clientFor(
  connection: GatewayConnection,
  resource: string,
): GatewayClient | null {
  const store = connections.tokensFor(connection.id);
  if (!store) {
    return null;
  }
  return new GatewayClient({
    baseUrl: connection.gatewayUrl,
    resource,
    tokens: { getAccessToken: (wanted) => store.getAccessToken(wanted) },
  });
}

/**
 * The runtime slug for this connection.
 *
 * Uncached, unlike `consoleClients.ts`: a tray press is a rare event, and a
 * headless launch would not see that module's cache anyway. Discovery failing
 * is not an error — `runtimeSlugFrom` falls back to a slug that mints a working
 * token, because the verbs bind to the caller rather than to an endpoint
 * (`runtimeSlug.ts`).
 */
async function resolveRuntimeSlug(
  connection: GatewayConnection,
): Promise<string> {
  let apps: AppListResponse | null = null;
  try {
    apps =
      (await clientFor(connection, RESOURCE_CONTROL)?.request<AppListResponse>(
        "/api/v1/cli/apps",
      )) ?? null;
  } catch {
    apps = null;
  }
  return runtimeSlugFrom(apps);
}

async function performDecisionAction(
  response: DecisionResponse,
  action: DecisionAction,
  connection: GatewayConnection,
  sandboxId: string,
  runName: string,
): Promise<void> {
  const [pendingTitle, pendingBody] = pendingCopy(action, runName);
  await presentFeedback(response, pendingTitle, pendingBody, true);
  try {
    const slug = await resolveRuntimeSlug(connection);
    const runtime = clientFor(connection, runtimeResource(slug));
    if (!runtime) {
      await degrade(response, action, runName);
      return;
    }
    const id = encodeURIComponent(sandboxId);
    if (action === "nudge") {
      await runtime.request(`/api/v1/runtime/sandboxes/${id}/messages`, {
        method: "POST",
        body: { body: NUDGE_MESSAGE_BODY, interrupt: true },
      });
      await presentFeedback(
        response,
        "Nudge sent",
        `${runName} was asked for a status update. It interrupts the current turn.`,
        false,
      );
      return;
    }
    await runtime.request(`/api/v1/runtime/sandboxes/${id}/cancel`, {
      method: "POST",
    });
    await presentFeedback(
      response,
      action === "accept_stop" ? "Accepted" : "Cancelling",
      action === "accept_stop"
        ? `${runName} is stopping; its completed output is kept.`
        : `${runName} is being cancelled.`,
      false,
    );
  } catch (error) {
    // The run may have ended between the push and the press. The gateway says
    // so rather than acting, and that is a calm "already finished" rather than
    // a failure worth asking the user to retry.
    if (
      hasErrorCode(error, "sandbox_already_terminal") ||
      hasErrorCode(error, "sandbox_unavailable")
    ) {
      await presentFeedback(
        response,
        "Already finished",
        `${runName} ended before your response arrived — nothing to do.`,
        false,
      );
      return;
    }
    // Includes the locked-device path: the credential lives in the keychain
    // and can be unreadable before first unlock, which surfaces here as a
    // failed request rather than a crash.
    await degrade(response, action, runName);
  }
}

/** What a press becomes when this session cannot carry it out. */
async function degrade(
  response: DecisionResponse,
  action: DecisionAction,
  runName: string,
): Promise<void> {
  await presentFeedback(
    response,
    action === "nudge" ? "Couldn't send the nudge" : "Couldn't cancel",
    `Open Tidebreak to act on ${runName}.`,
    false,
  );
}

/**
 * Executes one action-button response end to end. Returns true when the
 * response was a decision action this module owns — whether or not the request
 * succeeded — and false when it was not, leaving a plain tap for the caller to
 * route.
 */
export async function executeDecisionResponse(
  response: DecisionResponse,
): Promise<boolean> {
  const action = decisionAction(response.actionIdentifier);
  if (!action) {
    return false;
  }
  const sandboxId = pickString(response.data, "sandbox_id");
  if (!sandboxId) {
    return true;
  }
  const key = `${response.requestIdentifier}:${response.actionIdentifier}`;
  if (!(await claimNotificationResponse(secureStorage, key))) {
    return true;
  }
  const runName = pickString(response.data, "profile_name") ?? "The sandbox";
  try {
    // A killed-app press mounts no React tree, so nothing else has read the
    // registry. Hydrating is idempotent per process.
    await hydrateConnections();
    const connection = connectionFor(
      pickString(response.data, "installation_id"),
    );
    if (!connection) {
      return true;
    }
    if (!grantsRuntimeExecute(connection.grantedScope)) {
      // The session never consented to the sandbox verbs — a pairing against
      // a gateway that does not offer them to this client. The notification
      // still arrived, and its buttons are OS-attached, so say so plainly
      // instead of failing a request that was never going to be permitted.
      await degrade(response, action, runName);
      return true;
    }
    await performDecisionAction(
      response,
      action,
      connection,
      sandboxId,
      runName,
    );
  } catch {
    // A registry failure must never take a headless launch down over a press.
  }
  return true;
}
