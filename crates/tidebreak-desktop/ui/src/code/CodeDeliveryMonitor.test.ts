// @vitest-environment jsdom
import { createElement } from "react";
import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../api/client";
import type {
  CodeDeliveryPullRequestQuery,
  CodeDeliveryPullRequestSummary,
  CodeDeliveryRunQuery,
  CodeDeliveryRunSummary,
  CodeGitHubRepositoryRef,
} from "../api/types";
import {
  codeDeliveryRepositoryKey,
  resetCodeDeliveryHostState,
  useCodeDeliveryStore,
  type CodeDeliveryNotificationRule,
} from "./CodeDeliveryStore";
import { resetCodeClientGenerationForTests } from "./CodeClientGeneration";
import {
  CodeDeliveryMonitor,
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

function pullRequest(id: number): CodeDeliveryPullRequestSummary {
  return {
    id: `${codeDeliveryRepositoryKey(repository)}#${id}`,
    repository,
    number: id,
    url: `${repository.url}/pull/${id}`,
    title: `Pull request ${id}`,
    state: "open",
    draft: false,
    head_branch: `feature/${id}`,
    base_branch: "main",
    head_sha: `sha-${id}`,
    auto_merge_enabled: false,
    checks: [],
    attention_reasons: [],
    ready_to_merge: false,
    workspace_links: [],
    labels: [],
    created_at: "2026-09-10T12:00:00.000Z",
    updated_at: "2026-09-10T12:00:00.000Z",
  };
}

function run(id: number): CodeDeliveryRunSummary {
  return {
    id: `${codeDeliveryRepositoryKey(repository)}:workflow_run:${id}`,
    repository,
    kind: "workflow_run",
    github_id: id,
    name: `CI ${id}`,
    url: `${repository.url}/actions/runs/${id}`,
    status: "completed",
    conclusion: "success",
    attention_reasons: [],
    workspace_links: [],
    created_at: "2026-09-10T12:00:00.000Z",
    updated_at: "2026-09-10T12:00:00.000Z",
  };
}

beforeEach(() => {
  window.localStorage.clear();
  resetCodeClientGenerationForTests();
  useCodeDeliveryStore.setState({
    manualRepositories: [],
    excludedRegisteredRepoIds: [],
    pinnedRepositoryKeys: [],
    savedViews: [],
    lastPollAt: null,
    lastSuccessfulPollAt: null,
    knownAuthors: [],
    legacyNotificationRules: null,
    notificationRulesMigrated: false,
    persistenceError: null,
  });
  resetCodeDeliveryHostState();
});

afterEach(() => {
  cleanup();
  resetCodeDeliveryHostState();
  resetCodeClientGenerationForTests();
  window.localStorage.clear();
  vi.restoreAllMocks();
});

describe("migrateLegacyNotificationRules", () => {
  it("arms every mapped rule as a server notification trigger", async () => {
    const listCodeTriggers = vi.fn(async () => []);
    const createCodeTrigger = vi.fn(async () => ({}) as never);

    await expect(
      migrateLegacyNotificationRules(
        { listCodeTriggers, createCodeTrigger },
        [attentionRule],
        { repositories: [repository], errors: [] },
      ),
    ).resolves.toBe(true);

    expect(listCodeTriggers).toHaveBeenCalledWith("repo-1");
    expect(createCodeTrigger.mock.calls).toEqual([
      ["repo-1", "changes_requested", "notify"],
      ["repo-1", "conflicts", "notify"],
    ]);
  });

  it("keeps the migration pending when repository discovery is partial", async () => {
    const listCodeTriggers = vi.fn(async () => []);
    const createCodeTrigger = vi.fn(async () => ({}) as never);

    await expect(
      migrateLegacyNotificationRules(
        { listCodeTriggers, createCodeTrigger },
        [attentionRule],
        {
          repositories: [repository],
          errors: [{ kind: "github", message: "one repository failed" }],
        },
      ),
    ).resolves.toBe(false);

    expect(createCodeTrigger).toHaveBeenCalledTimes(2);
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
        { repositories: [repository], errors: [] },
      ),
    ).rejects.toThrow("offline");
    await migrateLegacyNotificationRules(
      { listCodeTriggers, createCodeTrigger },
      [attentionRule],
      { repositories: [repository], errors: [] },
    );

    expect(createCodeTrigger.mock.calls).toEqual([
      ["repo-1", "changes_requested", "notify"],
      ["repo-1", "conflicts", "notify"],
      ["repo-1", "conflicts", "notify"],
    ]);
  });
});

describe("CodeDeliveryMonitor", () => {
  it("carries three-page aggregates across two bounded passes", async () => {
    const pullRequests = [pullRequest(1), pullRequest(2), pullRequest(3)];
    const runs = [run(1), run(2), run(3)];
    const pullRequestCursors = [undefined, "pr-2", "pr-3"];
    const runCursors = [undefined, "run-2", "run-3"];
    let pullRequestPage = 0;
    let runPage = 0;
    const completeDeliveryPoll = vi.fn(
      useCodeDeliveryStore.getState().completeDeliveryPoll,
    );
    useCodeDeliveryStore.setState({ completeDeliveryPoll });

    const client = {
      getCodeDeliveryRepositories: vi.fn(async () => ({
        capability: { found: true, authenticated: true, remediation: "" },
        repositories: [repository],
        errors: [],
        fetched_at: "2026-09-10T12:00:00.000Z",
      })),
      queryCodeDeliveryPullRequests: vi.fn(
        async (query: CodeDeliveryPullRequestQuery) => {
          expect(query.cursor).toBe(pullRequestCursors[pullRequestPage]);
          if (pullRequestPage === 2) {
            expect(useCodeDeliveryStore.getState().lastPollAt).toBeNull();
          }
          const page = pullRequestPage;
          pullRequestPage += 1;
          return {
            capability: { found: true, authenticated: true, remediation: "" },
            items: [pullRequests[page]],
            errors: [],
            fetched_at: "2026-09-10T12:00:00.000Z",
            ...(page < 2 ? { next_cursor: pullRequestCursors[page + 1] } : {}),
          };
        },
      ),
      queryCodeDeliveryRuns: vi.fn(async (query: CodeDeliveryRunQuery) => {
        expect(query.cursor).toBe(runCursors[runPage]);
        if (runPage === 2) {
          expect(useCodeDeliveryStore.getState().lastPollAt).toBeNull();
        }
        const page = runPage;
        runPage += 1;
        return {
          capability: { found: true, authenticated: true, remediation: "" },
          items: [runs[page]],
          errors: [],
          fetched_at: "2026-09-10T12:00:00.000Z",
          ...(page < 2 ? { next_cursor: runCursors[page + 1] } : {}),
        };
      }),
    } as unknown as ApiClient;

    render(createElement(CodeDeliveryMonitor, { client, maxPagesPerPass: 2 }));

    await waitFor(() => expect(completeDeliveryPoll).toHaveBeenCalledOnce());
    expect(completeDeliveryPoll).toHaveBeenCalledWith(
      pullRequests,
      runs,
      expect.any(String),
    );
    expect(client.queryCodeDeliveryPullRequests).toHaveBeenCalledTimes(3);
    expect(client.queryCodeDeliveryRuns).toHaveBeenCalledTimes(3);
    expect(useCodeDeliveryStore.getState().lastPollAt).toBe(
      completeDeliveryPoll.mock.calls[0]?.[2],
    );
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

  it("caps each pass and returns the cursor for the next pass", async () => {
    let page = 0;
    const queryCodeDeliveryRuns = vi.fn(async () => {
      page += 1;
      return {
        capability: { found: true, authenticated: true, remediation: "" },
        items: [],
        errors: [],
        fetched_at: "2026-08-20T12:00:00.000Z",
        next_cursor: `cursor-${page}`,
      };
    });

    const batch = await monitorRuns(
      { queryCodeDeliveryRuns },
      [],
      "2026-08-20T00:00:00.000Z",
    );

    expect(queryCodeDeliveryRuns).toHaveBeenCalledTimes(5);
    expect(batch).toEqual({
      items: [],
      complete: false,
      nextCursor: "cursor-5",
    });
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
