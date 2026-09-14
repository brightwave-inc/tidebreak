import { describe, expect, it } from "vitest";
import type { HttpFetch, HttpResponse } from "./http";
import { memoryStorage } from "./storage";
import {
  applyPushPreference,
  claimNotificationResponse,
  fetchPushPreferences,
  gatewayDeliversPush,
  parsePushPreferences,
  pushKindCopy,
  pushPlatform,
  registerPushDevice,
  rendersDataMessages,
  resetHandledResponseCache,
  revokePushDevice,
  setPushPreference,
} from "./push";

function jsonResponse(status: number, body: unknown): HttpResponse {
  return {
    status,
    ok: status >= 200 && status < 300,
    redirected: false,
    url: "",
    json: async () => body,
    text: async () => JSON.stringify(body),
  };
}

function recordingFetch(response: HttpResponse) {
  const calls: {
    url: string;
    method?: string;
    headers?: Record<string, string>;
    body?: unknown;
  }[] = [];
  const fetchImpl: HttpFetch = async (url, init) => {
    calls.push({
      url,
      ...(init.method ? { method: init.method } : {}),
      ...(init.headers ? { headers: init.headers } : {}),
      ...(init.body === undefined ? {} : { body: JSON.parse(init.body) }),
    });
    return response;
  };
  return { calls, fetchImpl };
}

describe("gatewayDeliversPush", () => {
  it("only says yes when the installation advertises it", () => {
    // Absent means no: a dark installation refuses the device and preference
    // routes, so offering the controls would be offering a refusal.
    expect(gatewayDeliversPush({ surfaces: { push: true } })).toBe(true);
    expect(gatewayDeliversPush({ surfaces: { push: false } })).toBe(false);
    expect(gatewayDeliversPush({ surfaces: {} })).toBe(false);
    expect(gatewayDeliversPush({})).toBe(false);
    expect(gatewayDeliversPush(null)).toBe(false);
  });
});

describe("pushPlatform", () => {
  it.each([
    ["ios", "ios"],
    ["android", "android"],
  ])("maps %s through", (os, expected) => {
    expect(pushPlatform(os)).toBe(expected);
  });

  it("has no answer for web", () => {
    expect(pushPlatform("web")).toBeNull();
  });
});

describe("rendersDataMessages", () => {
  it("claims the capability only on Android with a Firebase config", () => {
    expect(rendersDataMessages("android", true)).toBe(true);
  });

  it("refuses it on an Android build with no Firebase config", () => {
    // This repository ships no google-services.json, so this is today's
    // Android. Claiming it would ask the gateway to send a data-only message
    // that never arrives and never renders — silence instead of a tap-only
    // notification.
    expect(rendersDataMessages("android", false)).toBe(false);
  });

  it("never claims it on iOS, where buttons come from the OS category", () => {
    expect(rendersDataMessages("ios", true)).toBe(false);
  });
});

describe("registerPushDevice", () => {
  it("upserts the device row with this connection's bearer", async () => {
    const { calls, fetchImpl } = recordingFetch(jsonResponse(200, {}));
    await registerPushDevice(
      "https://gateway.example/",
      "token-abc",
      {
        platform: "ios",
        expo_push_token: "ExponentPushToken[x]",
        device_token: "apns-token",
        renders_data_messages: false,
      },
      fetchImpl,
    );
    expect(calls[0]?.url).toBe("https://gateway.example/api/v1/cli/devices");
    expect(calls[0]?.method).toBe("POST");
    expect(calls[0]?.headers?.Authorization).toBe("Bearer token-abc");
    expect(calls[0]?.body).toEqual({
      platform: "ios",
      expo_push_token: "ExponentPushToken[x]",
      device_token: "apns-token",
      renders_data_messages: false,
    });
  });

  it("raises when the gateway refuses", async () => {
    const { fetchImpl } = recordingFetch(jsonResponse(404, {}));
    await expect(
      registerPushDevice(
        "https://gateway.example",
        "t",
        {
          platform: "ios",
          expo_push_token: "x",
          device_token: null,
          renders_data_messages: false,
        },
        fetchImpl,
      ),
    ).rejects.toThrow(/HTTP 404/);
  });
});

describe("revokePushDevice", () => {
  it("names the address to drop in a DELETE body", async () => {
    const { calls, fetchImpl } = recordingFetch(jsonResponse(204, null));
    await revokePushDevice(
      "https://gateway.example",
      "token-abc",
      "ExponentPushToken[x]",
      { fetchImpl },
    );
    expect(calls[0]?.method).toBe("DELETE");
    expect(calls[0]?.body).toEqual({ expo_push_token: "ExponentPushToken[x]" });
  });
});

describe("push preferences", () => {
  it("reads the gateway's list", async () => {
    const { calls, fetchImpl } = recordingFetch(
      jsonResponse(200, {
        preferences: [
          { kind: "task_complete", enabled: true },
          { kind: "daily_budget", enabled: false },
        ],
      }),
    );
    await expect(
      fetchPushPreferences("https://gateway.example", "t", fetchImpl),
    ).resolves.toEqual([
      { kind: "task_complete", enabled: true },
      { kind: "daily_budget", enabled: false },
    ]);
    expect(calls[0]?.url).toBe(
      "https://gateway.example/api/v1/cli/push-preferences",
    );
  });

  it("writes one kind at a time", async () => {
    const { calls, fetchImpl } = recordingFetch(jsonResponse(200, {}));
    await setPushPreference(
      "https://gateway.example",
      "t",
      "possibly_stalled",
      false,
      fetchImpl,
    );
    expect(calls[0]?.method).toBe("PUT");
    expect(calls[0]?.body).toEqual({
      kind: "possibly_stalled",
      enabled: false,
    });
  });
});

describe("parsePushPreferences", () => {
  it("keeps well-formed rows and drops the rest", () => {
    // The gateway owns the vocabulary and may grow it; one malformed row must
    // not cost the user every switch on the screen.
    expect(
      parsePushPreferences({
        preferences: [
          { kind: "task_complete", enabled: true },
          { kind: "", enabled: true },
          { kind: "no_flag" },
          null,
          "nonsense",
          { kind: "future_kind", enabled: false },
        ],
      }),
    ).toEqual([
      { kind: "task_complete", enabled: true },
      { kind: "future_kind", enabled: false },
    ]);
  });

  it.each([null, {}, { preferences: "no" }])(
    "reads %j as an empty list",
    (json) => {
      expect(parsePushPreferences(json)).toEqual([]);
    },
  );
});

describe("applyPushPreference", () => {
  const list = [
    { kind: "task_complete", enabled: true },
    { kind: "daily_budget", enabled: true },
  ];

  it("flips exactly one row and leaves the others alone", () => {
    expect(applyPushPreference(list, "daily_budget", false)).toEqual([
      { kind: "task_complete", enabled: true },
      { kind: "daily_budget", enabled: false },
    ]);
  });

  it("is a no-op for a kind that is not on the list", () => {
    expect(applyPushPreference(list, "unknown", false)).toEqual(list);
  });

  it("does not mutate the list it was given", () => {
    // The failure path restores the previous list by identity; mutating in
    // place would make the restore a no-op and leave a lying switch.
    const before = structuredClone(list);
    applyPushPreference(list, "task_complete", false);
    expect(list).toEqual(before);
  });
});

describe("pushKindCopy", () => {
  it("names the kinds this build knows", () => {
    expect(pushKindCopy("soft_ceiling_warned").title).toBe("Ceiling warning");
  });

  it("humanizes a kind added after this build shipped", () => {
    // Hiding an unknown kind would leave a notification the user cannot find
    // a switch for, which is worse than an imperfect label.
    expect(pushKindCopy("brand_new_kind")).toEqual({
      title: "Brand new kind",
      detail: "A new alert kind from this gateway.",
    });
  });
});

describe("claimNotificationResponse", () => {
  it("lets exactly one claim through per key", async () => {
    resetHandledResponseCache();
    const storage = memoryStorage();
    await expect(claimNotificationResponse(storage, "a:cancel")).resolves.toBe(
      true,
    );
    await expect(claimNotificationResponse(storage, "a:cancel")).resolves.toBe(
      false,
    );
    await expect(claimNotificationResponse(storage, "a:nudge")).resolves.toBe(
      true,
    );
  });

  it("refuses a key a previous process already recorded", async () => {
    // Android queues a killed-app button press and replays it to the UI
    // listeners on the next app open. Nudging twice is two interrupts into a
    // running turn, so the record has to survive the process that wrote it.
    const storage = memoryStorage();
    resetHandledResponseCache();
    await claimNotificationResponse(storage, "a:nudge");
    resetHandledResponseCache();
    await expect(claimNotificationResponse(storage, "a:nudge")).resolves.toBe(
      false,
    );
  });

  it("forgets the oldest keys rather than growing without bound", async () => {
    // expo-secure-store caps a value near 2KB on Android; an unbounded list
    // would eventually fail to write and silently stop deduping anything.
    const storage = memoryStorage();
    resetHandledResponseCache();
    for (let i = 0; i < 25; i += 1) {
      await claimNotificationResponse(storage, `key-${i}`);
    }
    resetHandledResponseCache();
    await expect(claimNotificationResponse(storage, "key-0")).resolves.toBe(
      true,
    );
    resetHandledResponseCache();
    await expect(claimNotificationResponse(storage, "key-24")).resolves.toBe(
      false,
    );
  });
});
