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
 */

import * as Notifications from "expo-notifications";
import { Platform } from "react-native";
import { fetchRefusingRedirects } from "../lib/http";
import { claimNotificationResponse } from "../lib/push";
import { grantsRuntimeExecute } from "../lib/scope";
import { RESOURCE_CONTROL, runtimeResource } from "../lib/resource";
import { validatedBaseUrl } from "../lib/url";
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
 * The MCP endpoint slug whose `runtime:<slug>` resource carries the owner
 * verbs, discovered from the member catalog. Deliberately not guessed: a
 * wrong slug is refused by the gateway, and a refusal the user cannot read is
 * worse than an honest "open the app".
 */
async function runtimeSlug(
  gatewayUrl: string,
  accessToken: string,
): Promise<string | null> {
  try {
    const response = await fetchRefusingRedirects(
      `${validatedBaseUrl(gatewayUrl)}/api/v1/cli/apps`,
      { headers: { Authorization: `Bearer ${accessToken}` } },
    );
    if (!response.ok) {
      return null;
    }
    const body = (await response.json()) as {
      apps?: { mcp_endpoint_slugs?: unknown }[];
    };
    for (const app of body.apps ?? []) {
      const slugs = app.mcp_endpoint_slugs;
      if (Array.isArray(slugs)) {
        const slug = slugs.find(
          (value): value is string =>
            typeof value === "string" && value.length > 0,
        );
        if (slug) {
          return slug;
        }
      }
    }
    return null;
  } catch {
    return null;
  }
}

async function runtimeRequest(
  gatewayUrl: string,
  accessToken: string,
  path: string,
  body?: unknown,
): Promise<void> {
  const response = await fetchRefusingRedirects(
    `${validatedBaseUrl(gatewayUrl)}${path}`,
    {
      method: "POST",
      headers: {
        Authorization: `Bearer ${accessToken}`,
        "Content-Type": "application/json",
        Accept: "application/json",
      },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    },
  );
  if (!response.ok) {
    throw new Error(`${path} failed (HTTP ${response.status})`);
  }
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
  const store = connections.tokensFor(connection.id);
  if (!store) {
    await degrade(response, action, runName);
    return;
  }
  try {
    const control = await store.getAccessToken(RESOURCE_CONTROL);
    const slug = await runtimeSlug(connection.gatewayUrl, control);
    if (!slug) {
      await degrade(response, action, runName);
      return;
    }
    const runtime = await store.getAccessToken(runtimeResource(slug));
    const id = encodeURIComponent(sandboxId);
    if (action === "nudge") {
      await runtimeRequest(
        connection.gatewayUrl,
        runtime,
        `/api/v1/runtime/sandboxes/${id}/messages`,
        { body: NUDGE_MESSAGE_BODY, interrupt: true },
      );
      await presentFeedback(
        response,
        "Nudge sent",
        `${runName} was asked for a status update. It interrupts the current turn.`,
        false,
      );
      return;
    }
    await runtimeRequest(
      connection.gatewayUrl,
      runtime,
      `/api/v1/runtime/sandboxes/${id}/cancel`,
    );
    await presentFeedback(
      response,
      action === "accept_stop" ? "Accepted" : "Cancelling",
      action === "accept_stop"
        ? `${runName} is stopping; its completed output is kept.`
        : `${runName} is being cancelled.`,
      false,
    );
  } catch {
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
