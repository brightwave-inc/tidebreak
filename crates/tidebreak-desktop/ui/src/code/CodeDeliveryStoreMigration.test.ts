// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const STORAGE_KEY = "tidebreak.code-delivery";

function legacyState() {
  return {
    version: 2,
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
    notifications: [{ id: "legacy-client-row" }],
    seenFingerprints: { "legacy-client-row": "2026-08-28T12:00:01Z" },
    lastPollAt: "2026-08-28T12:00:01Z",
    knownAuthors: [{ login: "mara" }],
  };
}

function pullRequestView(id: string) {
  return {
    id,
    kind: "pull_requests",
    name: `View ${id}`,
    createdAt: "2026-08-28T12:00:00Z",
    filters: {
      search: "",
      repositoryKeys: [],
      states: ["open"],
      reviewStates: [],
      checkStates: [],
      authors: [],
      attentionOnly: false,
      readyOnly: false,
    },
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
    useCodeDeliveryStore
      .getState()
      .rememberDeliveryAuthors([{ login: "devon" }]);
    expect(
      JSON.parse(window.localStorage.getItem(STORAGE_KEY) ?? "{}"),
    ).toMatchObject({ notificationRules: legacyState().notificationRules });
    expect(
      JSON.parse(window.localStorage.getItem(STORAGE_KEY) ?? "{}"),
    ).not.toHaveProperty("notifications");
    expect(
      JSON.parse(window.localStorage.getItem(STORAGE_KEY) ?? "{}"),
    ).not.toHaveProperty("seenFingerprints");

    expect(rules).not.toBeNull();
    useCodeDeliveryStore.getState().completeNotificationRuleMigration(rules!);
    const migrated = JSON.parse(
      window.localStorage.getItem(STORAGE_KEY) ?? "{}",
    );
    expect(migrated.notificationRulesMigrated).toBe(true);
    expect(migrated).not.toHaveProperty("notificationRules");

    const reloaded = await loadStore();
    expect(
      reloaded.useCodeDeliveryStore.getState().legacyNotificationRules,
    ).toBeNull();
  });

  it("does not invent migration completion when no saved state exists", async () => {
    const { useCodeDeliveryStore } = await loadStore();

    expect(useCodeDeliveryStore.getState().legacyNotificationRules).toBeNull();
    useCodeDeliveryStore
      .getState()
      .completeDeliveryPoll([], [], "2026-08-29T12:00:00Z");
    const persisted = JSON.parse(
      window.localStorage.getItem(STORAGE_KEY) ?? "{}",
    );
    expect(persisted).not.toHaveProperty("notificationRulesMigrated");
    expect(persisted).not.toHaveProperty("notificationRules");
  });

  it("drops one bad saved-view row while keeping every other field", async () => {
    window.localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        ...legacyState(),
        manualRepositories: [
          {
            host: "github.com",
            owner: "brightwave-inc",
            name: "tidebreak",
            name_with_owner: "brightwave-inc/tidebreak",
            url: "https://github.com/brightwave-inc/tidebreak",
          },
        ],
        pinnedRepositoryKeys: ["github.com/brightwave-inc/tidebreak"],
        savedViews: [
          pullRequestView("kept"),
          { ...pullRequestView("bad"), filters: null },
        ],
      }),
    );

    const { useCodeDeliveryStore } = await loadStore();
    const state = useCodeDeliveryStore.getState();
    expect(state.savedViews.map((view) => view.id)).toEqual(["kept"]);
    expect(state.manualRepositories).toHaveLength(1);
    expect(state.pinnedRepositoryKeys).toEqual([
      "github.com/brightwave-inc/tidebreak",
    ]);
    expect(state.knownAuthors).toEqual([{ login: "mara" }]);
    expect(state.legacyNotificationRules).toHaveLength(3);
  });

  it("ignores a corrupt notification row without marking migration complete", async () => {
    window.localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        ...legacyState(),
        notifications: [{ id: null }],
      }),
    );

    const { useCodeDeliveryStore } = await loadStore();
    const rules = useCodeDeliveryStore.getState().legacyNotificationRules;
    expect(rules).toHaveLength(3);

    useCodeDeliveryStore
      .getState()
      .rememberDeliveryAuthors([{ login: "devon" }]);
    const persisted = JSON.parse(
      window.localStorage.getItem(STORAGE_KEY) ?? "{}",
    );
    expect(persisted).not.toHaveProperty("notificationRulesMigrated");
    expect(persisted.notificationRules).toHaveLength(3);
    expect(persisted).not.toHaveProperty("notifications");
  });
});
