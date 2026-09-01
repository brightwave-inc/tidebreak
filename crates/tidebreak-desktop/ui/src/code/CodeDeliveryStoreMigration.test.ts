// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const STORAGE_KEY = "tidebreak.code-delivery";

function legacyState() {
  return {
    version: 1,
    manualRepositories: [],
    excludedRegisteredRepoIds: [],
    pinnedRepositoryKeys: [],
    savedViews: [],
    notificationRules: [
      {
        id: "pull_request_attention",
        enabled: true,
        repositoryKeys: [],
        tidebreakLinkedOnly: false,
      },
      {
        id: "pull_request_ready",
        enabled: false,
        repositoryKeys: ["github.com/brightwave-inc/tidebreak"],
        tidebreakLinkedOnly: true,
      },
      {
        id: "run_failure",
        enabled: true,
        repositoryKeys: [],
        tidebreakLinkedOnly: false,
      },
    ],
    notifications: [
      {
        id: "run-failure:1",
        fingerprint: "run-failure:1",
        rule: "run_failure",
        title: "CI failed",
        detail: "failure",
        repositoryName: "brightwave-inc/tidebreak",
        occurredAt: "2026-08-28T12:00:00Z",
        receivedAt: "2026-08-28T12:00:01Z",
        url: "https://github.com/brightwave-inc/tidebreak/actions/runs/1",
        target: {
          kind: "run",
          repository: {
            host: "github.com",
            owner: "brightwave-inc",
            name: "tidebreak",
          },
          runKind: "workflow_run",
          id: 1,
        },
      },
    ],
    seenFingerprints: { "run-failure:1": "2026-08-28T12:00:01Z" },
    lastPollAt: "2026-08-28T12:00:01Z",
    knownAuthors: [{ login: "mara" }],
  };
}

async function loadStore() {
  vi.resetModules();
  return import("./CodeDeliveryStore");
}

beforeEach(() => {
  window.localStorage.clear();
});

afterEach(() => {
  window.localStorage.clear();
  vi.resetModules();
});

describe("delivery notification rule migration storage", () => {
  it("keeps legacy rules until completion, then records the one-way migration", async () => {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(legacyState()));
    const { useCodeDeliveryStore } = await loadStore();
    const rules = useCodeDeliveryStore.getState().legacyNotificationRules;

    expect(rules).toHaveLength(3);
    useCodeDeliveryStore.getState().markAllNotificationsRead();
    expect(
      JSON.parse(window.localStorage.getItem(STORAGE_KEY) ?? "{}"),
    ).toMatchObject({
      notificationRules: legacyState().notificationRules,
      notifications: [{ id: "run-failure:1" }],
      seenFingerprints: {
        "run-failure:1": "2026-08-28T12:00:01Z",
      },
    });

    expect(rules).not.toBeNull();
    useCodeDeliveryStore.getState().completeNotificationRuleMigration(rules!);
    const migrated = JSON.parse(
      window.localStorage.getItem(STORAGE_KEY) ?? "{}",
    );
    expect(migrated.notificationRulesMigrated).toBe(true);
    expect(migrated).not.toHaveProperty("notificationRules");
    expect(migrated.notifications[0].id).toBe("run-failure:1");
    expect(migrated.seenFingerprints).toEqual({
      "run-failure:1": "2026-08-28T12:00:01Z",
    });

    const reloaded = await loadStore();
    expect(
      reloaded.useCodeDeliveryStore.getState().legacyNotificationRules,
    ).toBeNull();
  });

  it("does not invent legacy rules when no saved state exists", async () => {
    const { useCodeDeliveryStore } = await loadStore();

    expect(useCodeDeliveryStore.getState().legacyNotificationRules).toBeNull();
    useCodeDeliveryStore.getState().finishPoll("2026-08-29T12:00:00Z");
    const persisted = JSON.parse(
      window.localStorage.getItem(STORAGE_KEY) ?? "{}",
    );
    expect(persisted.notificationRulesMigrated).toBe(true);
    expect(persisted).not.toHaveProperty("notificationRules");
  });
});
