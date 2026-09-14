/**
 * Keeping each connection's push registration in line with reality.
 *
 * Registered while notification permission is granted and the gateway
 * advertises push; revoked from the recorded token once either stops being
 * true. Reconciled when the connection set changes and on every return to the
 * foreground, because both permission and the gateway's capability can change
 * while the app is away — an operator can enable push long after a connection
 * was paired, which is why capability is re-read live rather than trusted from
 * a pair-time snapshot.
 *
 * Every connection is reconciled, not just the active one: a phone holding two
 * gateways should hear from both, and registration is an idempotent upsert
 * minted with each connection's own bearer.
 */

import Constants from "expo-constants";
import * as Device from "expo-device";
import * as Notifications from "expo-notifications";
import { useEffect, useRef } from "react";
import { AppState, Platform } from "react-native";
import { fetchGatewayMeta } from "../lib/gateway";
import {
  gatewayDeliversPush,
  pushPlatform,
  registerPushDevice,
  rendersDataMessages,
  revokePushDevice,
} from "../lib/push";
import { RESOURCE_CONTROL } from "../lib/resource";
import { connections, secureStorage } from "../session/runtime";
import { useConnectionStore } from "../session/store";
import {
  ensureAndroidChannels,
  ensureNotificationCategories,
} from "./categories";
import { executeDecisionResponse } from "./decisionResponses";
import { ensureNotificationRenderTask } from "./renderTask";
import { clearPushToken, readPushToken, writePushToken } from "./tokenCache";

/** How long sign-out waits on a deregistration before giving up on it. */
const DEREGISTER_TIMEOUT_MS = 3000;

/**
 * Whether this binary carries a Firebase config, which is what decides
 * whether Android has an FCM registration at all.
 *
 * This repository ships no `google-services.json` yet, so on Android today
 * this is false everywhere: the app registers with its Expo token only, tells
 * the gateway it cannot render data messages, and receives the ordinary
 * display-form notification without action buttons. iOS is unaffected — its
 * buttons come from the OS-native category match.
 */
function firebaseConfigured(): boolean {
  const android = Constants.expoConfig?.android as
    | { googleServicesFile?: unknown }
    | undefined;
  return typeof android?.googleServicesFile === "string";
}

/** Permission check that only prompts when asked to, and still allowed to. */
async function ensurePermission(prompt: boolean): Promise<boolean> {
  const current = await Notifications.getPermissionsAsync();
  if (current.granted) {
    return true;
  }
  if (!prompt || !current.canAskAgain) {
    return false;
  }
  return (await Notifications.requestPermissionsAsync()).granted;
}

async function fetchDeviceTokens(): Promise<{
  expoPushToken: string;
  deviceToken: string | null;
} | null> {
  if (!Device.isDevice) {
    return null;
  }
  const extra = Constants.expoConfig?.extra as
    | { eas?: { projectId?: unknown } }
    | undefined;
  const projectId = extra?.eas?.projectId;
  if (typeof projectId !== "string" || projectId.length === 0) {
    return null;
  }
  let expoPushToken: string;
  try {
    expoPushToken = (await Notifications.getExpoPushTokenAsync({ projectId }))
      .data;
  } catch {
    return null;
  }
  let deviceToken: string | null = null;
  try {
    deviceToken = (await Notifications.getDevicePushTokenAsync())
      .data as string;
  } catch {
    // No APNs registration, or no FCM config in this build. The Expo token is
    // enough for the display form.
  }
  return { expoPushToken, deviceToken };
}

async function controlToken(connectionId: string): Promise<string | null> {
  try {
    return (await connections.tokensFor(connectionId)?.getAccessToken(
      RESOURCE_CONTROL,
    )) ?? null;
  } catch {
    return null;
  }
}

/** Brings one connection's registration in line. */
async function reconcileConnection(
  connectionId: string,
  gatewayUrl: string,
  prompt: boolean,
  cancelled: () => boolean,
): Promise<void> {
  const platform = pushPlatform(Platform.OS);
  if (!platform) {
    return;
  }
  let deliversPush: boolean;
  try {
    deliversPush = gatewayDeliversPush(await fetchGatewayMeta(gatewayUrl));
  } catch {
    // A gateway that cannot be reached says nothing either way; leave the
    // registration as it stands and try again on the next foreground.
    return;
  }
  if (!deliversPush || cancelled()) {
    return;
  }
  const granted = await ensurePermission(prompt);
  if (cancelled()) {
    return;
  }
  if (!granted) {
    await deregisterConnection(connectionId, gatewayUrl);
    return;
  }
  const tokens = await fetchDeviceTokens();
  if (!tokens || cancelled()) {
    return;
  }
  const accessToken = await controlToken(connectionId);
  if (!accessToken || cancelled()) {
    return;
  }
  try {
    await registerPushDevice(gatewayUrl, accessToken, {
      platform,
      expo_push_token: tokens.expoPushToken,
      device_token: tokens.deviceToken,
      renders_data_messages: rendersDataMessages(
        Platform.OS,
        firebaseConfigured(),
      ),
    });
  } catch {
    // Retried on the next foreground.
    return;
  }
  await writePushToken(secureStorage, connectionId, tokens.expoPushToken);
}

/**
 * Drops one connection's device row, best-effort and time-bounded.
 *
 * Ordered *before* the credential is revoked, not after: the DELETE needs a
 * live `control` bearer, and a connection whose refresh family has already
 * been forgotten can no longer mint one. Getting this backwards leaves a dead
 * push address on the gateway addressed to a phone that has signed out.
 */
export async function deregisterConnection(
  connectionId: string,
  gatewayUrl: string,
): Promise<void> {
  const token = await readPushToken(secureStorage, connectionId);
  if (!token) {
    return;
  }
  const accessToken = await controlToken(connectionId);
  if (accessToken) {
    const abort = new AbortController();
    const timer = setTimeout(() => abort.abort(), DEREGISTER_TIMEOUT_MS);
    try {
      await revokePushDevice(gatewayUrl, accessToken, token, {
        signal: abort.signal,
      });
    } catch {
      // Best-effort; the gateway reaps addresses that stop accepting pushes.
    } finally {
      clearTimeout(timer);
    }
  }
  await clearPushToken(secureStorage, connectionId);
}

/**
 * Foreground presentation. Without this a notification that arrives while the
 * app is open is delivered silently to the listener and never shown, which
 * reads on a device as "push is broken".
 */
Notifications.setNotificationHandler({
  handleNotification: async () => ({
    shouldShowBanner: true,
    shouldShowList: true,
    shouldPlaySound: true,
    shouldSetBadge: false,
  }),
});

/**
 * Null-rendering syncer, mounted once at the app root. Only the first capable
 * reconcile of a session may prompt for permission — a prompt on every
 * foreground would be nagging, and iOS only ever shows it once anyway.
 */
export function PushSync(): null {
  const hydrated = useConnectionStore((state) => state.hydrated);
  const list = useConnectionStore((state) => state.connections);
  const firstReconcile = useRef(true);
  // Reconciliation depends on which gateways exist and where they live, not on
  // the array identity the store hands out on every unrelated change — an
  // identity that every machine attach and every identity refresh touches.
  const targets = list.map((connection) => ({
    id: connection.id,
    gatewayUrl: connection.gatewayUrl,
  }));
  const fingerprint = targets
    .map((target) => `${target.id}@${target.gatewayUrl}`)
    .join("|");

  useEffect(() => {
    void ensureAndroidChannels();
    // Categories and the render task re-register on every start, so a change
    // to either takes effect over the air rather than needing a reinstall.
    void ensureNotificationCategories();
    void ensureNotificationRenderTask();
  }, []);

  useEffect(() => {
    // Action presses that reach a running app: every iOS press (the OS wakes
    // the app into it) and Android presses while the app is foregrounded.
    // Android's backgrounded and killed-app presses arrive through the task
    // instead, and the executor dedupes across both.
    const subscription = Notifications.addNotificationResponseReceivedListener(
      (response) => {
        void executeDecisionResponse({
          actionIdentifier: response.actionIdentifier,
          requestIdentifier: response.notification.request.identifier,
          data: (response.notification.request.content.data ??
            {}) as Record<string, unknown>,
        });
      },
    );
    return () => subscription.remove();
  }, []);

  useEffect(() => {
    if (!hydrated || targets.length === 0) {
      return;
    }
    let cancelled = false;
    const reconcileAll = async () => {
      for (const target of targets) {
        if (cancelled) {
          return;
        }
        // Only the first capable reconcile of a session may prompt: a prompt
        // on every foreground would be nagging, and iOS shows it once anyway.
        const prompt = firstReconcile.current;
        firstReconcile.current = false;
        await reconcileConnection(
          target.id,
          target.gatewayUrl,
          prompt,
          () => cancelled,
        );
      }
    };
    void reconcileAll();
    const subscription = AppState.addEventListener("change", (state) => {
      if (state === "active") {
        void reconcileAll();
      }
    });
    return () => {
      cancelled = true;
      subscription.remove();
    };
    // Keyed on `fingerprint` rather than on `targets`: the store hands out a
    // fresh array on every emission, and most emissions change no gateway at
    // all — re-running the whole reconcile for each would be pointless work
    // and, worse, would re-enter it while the previous pass was still running.
  }, [hydrated, fingerprint]);

  return null;
}
