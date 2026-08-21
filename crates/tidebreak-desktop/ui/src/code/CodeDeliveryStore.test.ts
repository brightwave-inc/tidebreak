// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type {
  CodeDeliveryPullRequestSummary,
  CodeDeliveryRunSummary,
  CodeGitHubRepositoryRef,
} from "../api/types";
import {
  codeDeliveryRepositoryKey,
  trackedCodeDeliveryRepositories,
  unreadCodeDeliveryNotifications,
  useCodeDeliveryStore,
} from "./CodeDeliveryStore";

const NOW = "2026-08-20T12:00:00.000Z";

function repository(
  owner: string,
  name: string,
  tidebreakRepoId?: string,
): CodeGitHubRepositoryRef {
  return {
    host: "github.com",
    owner,
    name,
    name_with_owner: `${owner}/${name}`,
    url: `https://github.com/${owner}/${name}`,
    default_branch: "main",
    ...(tidebreakRepoId ? { tidebreak_repo_id: tidebreakRepoId } : {}),
  };
}

function pullRequest(
  id: number,
  repo: CodeGitHubRepositoryRef,
  overrides: Partial<CodeDeliveryPullRequestSummary> = {},
): CodeDeliveryPullRequestSummary {
  return {
    id: `${codeDeliveryRepositoryKey(repo)}#${id}`,
    repository: repo,
    number: id,
    url: `${repo.url}/pull/${id}`,
    title: `Pull request ${id}`,
    state: "open",
    draft: false,
    head_branch: `feature/${id}`,
    base_branch: "main",
    head_sha: `sha-${id}`,
    auto_merge_enabled: false,
    checks: [],
    attention_reasons: ["checks_failed"],
    ready_to_merge: false,
    workspace_links: [],
    labels: [],
    created_at: NOW,
    updated_at: NOW,
    ...overrides,
  };
}

function run(
  id: number,
  repo: CodeGitHubRepositoryRef,
  overrides: Partial<CodeDeliveryRunSummary> = {},
): CodeDeliveryRunSummary {
  return {
    id: `${codeDeliveryRepositoryKey(repo)}:workflow_run:${id}`,
    repository: repo,
    kind: "workflow_run",
    github_id: id,
    name: `CI ${id}`,
    url: `${repo.url}/actions/runs/${id}`,
    status: "completed",
    conclusion: "failure",
    attention_reasons: ["failure"],
    workspace_links: [],
    created_at: NOW,
    updated_at: NOW,
    ...overrides,
  };
}

beforeEach(() => {
  window.localStorage.clear();
  useCodeDeliveryStore.getState().reset();
});

afterEach(() => {
  useCodeDeliveryStore.getState().reset();
  window.localStorage.clear();
});

describe("trackedCodeDeliveryRepositories", () => {
  it("combines registered and manual repositories, excludes opted-out rows, and pins first", () => {
    const alpha = repository("brightwave-inc", "alpha", "repo-alpha");
    const zeta = repository("brightwave-inc", "zeta", "repo-zeta");
    const beta = repository("other-org", "beta");
    const store = useCodeDeliveryStore.getState();

    store.rememberManualRepositories([
      beta,
      { ...alpha, tidebreak_repo_id: undefined },
    ]);
    store.setRegisteredRepositoryExcluded("repo-zeta", true);
    store.setRepositoryPinned(codeDeliveryRepositoryKey(beta), true);

    const tracked = trackedCodeDeliveryRepositories([zeta, alpha], {
      manualRepositories: useCodeDeliveryStore.getState().manualRepositories,
      excludedRegisteredRepoIds:
        useCodeDeliveryStore.getState().excludedRegisteredRepoIds,
      pinnedRepositoryKeys:
        useCodeDeliveryStore.getState().pinnedRepositoryKeys,
    });

    expect(tracked.map((item) => item.name_with_owner)).toEqual([
      "other-org/beta",
      "brightwave-inc/alpha",
    ]);
    expect(
      JSON.parse(
        window.localStorage.getItem("tidebreak.code-delivery") ?? "{}",
      ),
    ).toMatchObject({
      version: 1,
      excludedRegisteredRepoIds: ["repo-zeta"],
      pinnedRepositoryKeys: ["github.com/other-org/beta"],
    });
  });
});

describe("delivery notifications", () => {
  it("deduplicates a poll fingerprint and preserves explicit read state", () => {
    const repo = repository("brightwave-inc", "tidebreak", "repo-1");
    const store = useCodeDeliveryStore.getState();
    const item = pullRequest(2248, repo);

    expect(store.ingestDeliveryPoll([item], [], NOW)).toBe(1);
    expect(
      useCodeDeliveryStore.getState().ingestDeliveryPoll([item], [], NOW),
    ).toBe(0);
    expect(useCodeDeliveryStore.getState().notifications).toHaveLength(1);
    expect(
      unreadCodeDeliveryNotifications(useCodeDeliveryStore.getState()),
    ).toBe(1);

    const notificationId = useCodeDeliveryStore.getState().notifications[0]!.id;
    useCodeDeliveryStore.getState().markNotificationRead(notificationId);
    expect(
      unreadCodeDeliveryNotifications(useCodeDeliveryStore.getState()),
    ).toBe(0);

    useCodeDeliveryStore.getState().markNotificationRead(notificationId, false);
    expect(
      unreadCodeDeliveryNotifications(useCodeDeliveryStore.getState()),
    ).toBe(1);

    useCodeDeliveryStore.getState().markAllNotificationsRead();
    expect(
      unreadCodeDeliveryNotifications(useCodeDeliveryStore.getState()),
    ).toBe(0);
  });

  it("applies repository and Tidebreak-link scopes before creating feed rows", () => {
    const alpha = repository("brightwave-inc", "alpha", "repo-alpha");
    const beta = repository("brightwave-inc", "beta", "repo-beta");
    useCodeDeliveryStore
      .getState()
      .updateNotificationRule("pull_request_attention", {
        repositoryKeys: [codeDeliveryRepositoryKey(alpha)],
        tidebreakLinkedOnly: true,
      });

    expect(
      useCodeDeliveryStore
        .getState()
        .ingestDeliveryPoll([pullRequest(1, beta)], [], NOW),
    ).toBe(0);
    expect(
      useCodeDeliveryStore
        .getState()
        .ingestDeliveryPoll([pullRequest(2, alpha)], [], NOW),
    ).toBe(0);

    const linked = pullRequest(3, alpha, {
      workspace_links: [
        {
          workspace_id: "ws-3",
          repo_id: "repo-alpha",
          title: "Fix alpha",
          branch_name: "feature/3",
          status: "active",
          exact: true,
        },
      ],
    });
    expect(
      useCodeDeliveryStore.getState().ingestDeliveryPoll([linked], [], NOW),
    ).toBe(1);
    expect(useCodeDeliveryStore.getState().notifications[0]?.workspaceId).toBe(
      "ws-3",
    );
  });

  it("keeps at most 500 notifications and drops events older than 30 days", () => {
    const repo = repository("brightwave-inc", "tidebreak", "repo-1");
    const recent = Array.from({ length: 506 }, (_, index) =>
      run(index + 1, repo),
    );
    const old = run(999, repo, {
      created_at: "2026-07-20T11:59:59.000Z",
      updated_at: "2026-07-20T11:59:59.000Z",
    });

    expect(
      useCodeDeliveryStore.getState().ingestDeliveryPoll([], [old], NOW),
    ).toBe(0);
    expect(
      useCodeDeliveryStore.getState().ingestDeliveryPoll([], recent, NOW),
    ).toBe(506);
    expect(useCodeDeliveryStore.getState().notifications).toHaveLength(500);
  });
});
