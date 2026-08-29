import { describe, expect, it, vi } from "vitest";

import type {
  CodeDeliveryRunQuery,
  CodeGitHubRepositoryRef,
} from "../api/types";
import type { CodeDeliveryNotificationRule } from "./CodeDeliveryStore";
import {
  migrateLegacyNotificationRules,
  monitorRuns,
  monitorSince,
  nextMonitorDelayMs,
} from "./CodeDeliveryMonitor";

const repository: CodeGitHubRepositoryRef = {
  host: "github.com",
  owner: "brightwave-inc",
  name: "tidebreak",
  name_with_owner: "brightwave-inc/tidebreak",
  url: "https://github.com/brightwave-inc/tidebreak",
  tidebreak_repo_id: "repo-1",
};

const attentionRule: CodeDeliveryNotificationRule = {
  id: "pull_request_attention",
  enabled: true,
  repositoryKeys: [],
  tidebreakLinkedOnly: false,
};

describe("migrateLegacyNotificationRules", () => {
  it("arms every mapped rule as a server notification trigger", async () => {
    const listCodeTriggers = vi.fn(async () => []);
    const createCodeTrigger = vi.fn(async () => ({}) as never);

    await migrateLegacyNotificationRules(
      { listCodeTriggers, createCodeTrigger },
      [attentionRule],
      [repository],
    );

    expect(listCodeTriggers).toHaveBeenCalledWith("repo-1");
    expect(createCodeTrigger.mock.calls).toEqual([
      ["repo-1", "changes_requested", "notify"],
      ["repo-1", "conflicts", "notify"],
    ]);
  });

  it("retries only missing rows after a partial migration", async () => {
    const listCodeTriggers = vi
      .fn()
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([
        {
          id: "trigger-existing",
          repo_id: "repo-1",
          condition: "changes_requested",
          action: "notify",
          enabled: false,
          created_at: "2026-08-29T12:00:00Z",
          updated_at: "2026-08-29T12:01:00Z",
        },
      ]);
    const createCodeTrigger = vi
      .fn()
      .mockResolvedValueOnce({})
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce({});

    await expect(
      migrateLegacyNotificationRules(
        { listCodeTriggers, createCodeTrigger },
        [attentionRule],
        [repository],
      ),
    ).rejects.toThrow("offline");
    await migrateLegacyNotificationRules(
      { listCodeTriggers, createCodeTrigger },
      [attentionRule],
      [repository],
    );

    expect(createCodeTrigger.mock.calls).toEqual([
      ["repo-1", "changes_requested", "notify"],
      ["repo-1", "conflicts", "notify"],
      ["repo-1", "conflicts", "notify"],
    ]);
  });
});

describe("nextMonitorDelayMs", () => {
  it("has no safety or hidden poll clock", () => {
    expect(nextMonitorDelayMs({ rerunRequested: false })).toBeNull();
  });

  it("reruns immediately when a pass was skipped because one was already running", () => {
    expect(nextMonitorDelayMs({ rerunRequested: true })).toBe(0);
  });
});

describe("monitorRuns", () => {
  it("asks only for persisted workflow runs", async () => {
    const queries: CodeDeliveryRunQuery[] = [];
    await monitorRuns(
      {
        queryCodeDeliveryRuns: async (query) => {
          queries.push(query);
          return {
            capability: { found: false, remediation: "test" },
            items: [],
            errors: [],
            fetched_at: "2026-08-20T12:00:00.000Z",
          };
        },
      },
      [],
      "2026-08-20T00:00:00.000Z",
    );
    expect(queries).toHaveLength(1);
    expect(queries[0]?.kinds).toEqual(["workflow_run"]);
  });
});

describe("monitorSince", () => {
  const now = Date.parse("2026-08-20T12:00:00.000Z");

  it("looks back 24 hours before the first successful poll", () => {
    expect(monitorSince(null, now)).toBe("2026-08-19T12:00:00.000Z");
    expect(monitorSince("not-a-timestamp", now)).toBe(
      "2026-08-19T12:00:00.000Z",
    );
  });

  it("overlaps later polls by two minutes", () => {
    expect(monitorSince("2026-08-20T11:15:00.000Z", now)).toBe(
      "2026-08-20T11:13:00.000Z",
    );
  });

  it("never asks for more than 30 days of history", () => {
    expect(monitorSince("2026-01-01T00:00:00.000Z", now)).toBe(
      "2026-07-21T12:00:00.000Z",
    );
  });
});
