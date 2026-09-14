/**
 * Push, from the gateway's side of the wire (mg ADR 0093).
 *
 * Three facts shape this module.
 *
 * **Registration is per connection, not per device.** A phone may hold several
 * gateway connections; each one gets its own device row, minted with that
 * connection's own `control` bearer. Nothing here reaches for an active
 * connection — the caller names which one it is registering.
 *
 * **Preferences are per account, not per device.** Suppression happens at the
 * gateway's enqueue site, so a flipped toggle affects every device the account
 * is signed into. The gateway owns the kind vocabulary and returns every kind
 * it can send, so a build that predates a kind still renders a row for it.
 *
 * **A dark installation answers 404.** `surfaces.push` is the advertisement to
 * gate on; callers that skip the gate get a refusal, not a silent no-op.
 */

import { fetchRefusingRedirects, type HttpFetch } from "./http";
import type { SecureStorage } from "./storage";
import type { GatewayMeta } from "./types";
import { validatedBaseUrl } from "./url";

/** One kind's effective setting, as the gateway reports it. */
export type PushPreference = {
  kind: string;
  enabled: boolean;
};

/** `POST /api/v1/cli/devices` — one connection's push address. */
export type DeviceRegistration = {
  platform: "ios" | "android";
  expo_push_token: string;
  device_token: string | null;
  /**
   * Whether this build renders data-only pushes itself. Gates the gateway's
   * data-only delivery form for the decision kinds; a build that claims this
   * falsely gets silence, because nothing else would draw the notification.
   */
  renders_data_messages: boolean;
};

/** Whether this installation delivers push at all. */
export function gatewayDeliversPush(meta: GatewayMeta | null | undefined): boolean {
  return meta?.surfaces?.push === true;
}

/** The registration platform, or null where push has no meaning (web). */
export function pushPlatform(os: string): "ios" | "android" | null {
  return os === "ios" || os === "android" ? os : null;
}

/**
 * Whether this build can render the gateway's data-only decision pushes.
 *
 * Android only — iOS attaches action buttons OS-natively from the display
 * form — and only with a Firebase config in the binary. Without
 * `google-services.json` an Android build has no FCM registration at all, so
 * claiming the capability would ask the gateway to send a message that never
 * arrives and never renders. Reported `false` there, which downgrades Android
 * to today's tap-only display-form notifications rather than losing them.
 */
export function rendersDataMessages(
  os: string,
  firebaseConfigured: boolean,
): boolean {
  return os === "android" && firebaseConfigured;
}

async function cliRequest(
  gatewayUrl: string,
  path: string,
  accessToken: string,
  init: { method: string; body?: unknown; signal?: AbortSignal },
  fetchImpl?: HttpFetch,
): Promise<unknown> {
  const base = validatedBaseUrl(gatewayUrl);
  const response = await fetchRefusingRedirects(
    `${base}${path}`,
    {
      method: init.method,
      headers: {
        Authorization: `Bearer ${accessToken}`,
        "Content-Type": "application/json",
        Accept: "application/json",
      },
      ...(init.body === undefined
        ? {}
        : { body: JSON.stringify(init.body) }),
      ...(init.signal ? { signal: init.signal } : {}),
    },
    fetchImpl,
  );
  if (!response.ok) {
    throw new Error(`${path} failed (HTTP ${response.status})`);
  }
  try {
    return await response.json();
  } catch {
    return null;
  }
}

/** Idempotent upsert of this connection's device row. */
export async function registerPushDevice(
  gatewayUrl: string,
  accessToken: string,
  body: DeviceRegistration,
  fetchImpl?: HttpFetch,
): Promise<void> {
  await cliRequest(
    gatewayUrl,
    "/api/v1/cli/devices",
    accessToken,
    { method: "POST", body },
    fetchImpl,
  );
}

/**
 * Drops one push address. `DELETE` with a body, as the gateway defines it.
 * The `signal` exists for sign-out, which must not hang on a dead gateway.
 */
export async function revokePushDevice(
  gatewayUrl: string,
  accessToken: string,
  expoPushToken: string,
  options?: { signal?: AbortSignal; fetchImpl?: HttpFetch },
): Promise<void> {
  await cliRequest(
    gatewayUrl,
    "/api/v1/cli/devices",
    accessToken,
    {
      method: "DELETE",
      body: { expo_push_token: expoPushToken },
      ...(options?.signal ? { signal: options.signal } : {}),
    },
    options?.fetchImpl,
  );
}

export async function fetchPushPreferences(
  gatewayUrl: string,
  accessToken: string,
  fetchImpl?: HttpFetch,
): Promise<PushPreference[]> {
  return parsePushPreferences(
    await cliRequest(
      gatewayUrl,
      "/api/v1/cli/push-preferences",
      accessToken,
      { method: "GET" },
      fetchImpl,
    ),
  );
}

export async function setPushPreference(
  gatewayUrl: string,
  accessToken: string,
  kind: string,
  enabled: boolean,
  fetchImpl?: HttpFetch,
): Promise<void> {
  await cliRequest(
    gatewayUrl,
    "/api/v1/cli/push-preferences",
    accessToken,
    { method: "PUT", body: { kind, enabled } },
    fetchImpl,
  );
}

/**
 * Reads the preference list, keeping only well-formed rows rather than
 * failing the screen over one unknown entry — the gateway may add kinds this
 * build has never heard of, and that is the designed direction of drift.
 */
export function parsePushPreferences(json: unknown): PushPreference[] {
  const list =
    json && typeof json === "object"
      ? (json as Record<string, unknown>).preferences
      : null;
  if (!Array.isArray(list)) {
    return [];
  }
  return list.flatMap((entry) => {
    if (!entry || typeof entry !== "object") {
      return [];
    }
    const row = entry as Record<string, unknown>;
    return typeof row.kind === "string" &&
      row.kind.length > 0 &&
      typeof row.enabled === "boolean"
      ? [{ kind: row.kind, enabled: row.enabled }]
      : [];
  });
}

/**
 * The optimistic local edit a toggle applies before the write lands. A Switch
 * that snaps back a second later reads as broken, so the row flips first and
 * the failure path restores the previous list.
 */
export function applyPushPreference(
  preferences: PushPreference[],
  kind: string,
  enabled: boolean,
): PushPreference[] {
  return preferences.map((preference) =>
    preference.kind === kind ? { ...preference, enabled } : preference,
  );
}

/**
 * Human copy for a notified kind. The gateway owns the vocabulary, so an
 * unlisted kind still gets a row under its humanized slug rather than being
 * hidden — a notification the user cannot find a switch for is worse than an
 * imperfect label.
 */
const KIND_COPY: Record<string, { title: string; detail: string }> = {
  task_complete: { title: "Task complete", detail: "A run finished its task." },
  possibly_stalled: {
    title: "Possibly stalled",
    detail: "A run stopped making progress.",
  },
  acceptance_met: {
    title: "Acceptance met",
    detail: "A run met its acceptance criteria.",
  },
  soft_ceiling_warned: {
    title: "Ceiling warning",
    detail: "A run is close to its time or spend ceiling.",
  },
  daily_budget: {
    title: "Daily sandbox budget",
    detail: "Daily sandbox spend is nearing its budget.",
  },
};

export function pushKindCopy(kind: string): { title: string; detail: string } {
  const known = KIND_COPY[kind];
  if (known) {
    return known;
  }
  const words = kind.replace(/_/g, " ");
  return {
    title: words.charAt(0).toUpperCase() + words.slice(1),
    detail: "A new alert kind from this gateway.",
  };
}

const HANDLED_KEY = "tidebreak.mobile.push.handled";

/**
 * Small on purpose. expo-secure-store caps a value near 2KB on Android, and
 * the only duplicates that matter are the ones Android replays on the next app
 * open — a handful, not a history.
 */
const HANDLED_LIMIT = 20;

let handledCache: string[] | null = null;

/** Test seam; nothing in the app resets the process-lifetime cache. */
export function resetHandledResponseCache(): void {
  handledCache = null;
}

async function loadHandled(storage: SecureStorage): Promise<string[]> {
  if (handledCache) {
    return handledCache;
  }
  try {
    const raw = await storage.getItem(HANDLED_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : [];
    handledCache = Array.isArray(parsed)
      ? parsed.filter((entry): entry is string => typeof entry === "string")
      : [];
  } catch {
    handledCache = [];
  }
  return handledCache;
}

/**
 * Records a notification response as handled *before* it is acted on, and says
 * whether this process is the one that gets to act.
 *
 * Persistent rather than per-process, because Android queues a killed-app
 * response and re-delivers it to the UI listeners on the next app open: one
 * press can legitimately reach JS twice, in two different processes. Cancelling
 * twice is noise; nudging twice is two interrupts into a running turn.
 */
export async function claimNotificationResponse(
  storage: SecureStorage,
  key: string,
): Promise<boolean> {
  const handled = await loadHandled(storage);
  if (handled.includes(key)) {
    return false;
  }
  handled.push(key);
  if (handled.length > HANDLED_LIMIT) {
    handled.splice(0, handled.length - HANDLED_LIMIT);
  }
  try {
    await storage.setItem(HANDLED_KEY, JSON.stringify(handled));
  } catch {
    // The in-memory record still guards this process.
  }
  return true;
}
