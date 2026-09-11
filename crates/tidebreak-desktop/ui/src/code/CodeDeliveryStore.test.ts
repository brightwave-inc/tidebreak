// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type {
  CodeDeliveryPullRequestSummary,
  CodeDeliveryRunSummary,
  CodeGitHubRepositoryRef,
} from "../api/types";
import {
  codeDeliveryRepositoryKey,
  deliveryPullRequestPageKey,
  mergeKnownAuthors,
  rememberedPullRequestPage,
  resetCodeDeliveryHostState,
  trackedCodeDeliveryRepositories,
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
  useCodeDeliveryStore.setState({
    manualRepositories: [],
    excludedRegisteredRepoIds: [],
    pinnedRepositoryKeys: [],
    savedViews: [],
    lastPollAt: null,
    knownAuthors: [],
    legacyNotificationRules: null,
    notificationRulesMigrated: false,
    persistenceError: null,
  });
  resetCodeDeliveryHostState();
});

afterEach(() => {
  resetCodeDeliveryHostState();
  window.localStorage.clear();
});

describe("trackedCodeDeliveryRepositories", () => {
  it("clears host data without deleting Delivery preferences", () => {
    const manual = repository("other-org", "manual");
    useCodeDeliveryStore.getState().rememberManualRepositories([manual]);
    useCodeDeliveryStore.setState({
      polling: true,
      monitorError: "old host failed",
      repositorySnapshot: {
        capability: { found: true, authenticated: true, remediation: "" },
        repositories: [repository("brightwave-inc", "old", "repo-old")],
        errors: [],
        fetched_at: NOW,
      },
      repositoryLoading: true,
      repositoryError: "stale",
      repositoryFetchedAt: Date.now(),
    });

    resetCodeDeliveryHostState();

    expect(useCodeDeliveryStore.getState()).toMatchObject({
      manualRepositories: [manual],
      polling: false,
      monitorError: null,
      repositorySnapshot: null,
      repositoryLoading: false,
      repositoryError: null,
      repositoryFetchedAt: null,
      lastPullRequestPages: [],
    });
  });

  it("keeps the last successful pull-request page for a query key", () => {
    const repo = repository("brightwave-inc", "tidebreak", "repo-1");
    const key = deliveryPullRequestPageKey([codeDeliveryRepositoryKey(repo)], {
      search: "",
      repositoryKeys: [],
      states: ["open"],
      reviewStates: [],
      checkStates: [],
      authors: ["mara"],
      attentionOnly: false,
      readyOnly: false,
    });
    const item = pullRequest(41, repo);
    useCodeDeliveryStore.getState().rememberPullRequestPage({
      key,
      items: [item],
      fetchedAt: NOW,
      errors: [],
    });
    expect(
      rememberedPullRequestPage(
        useCodeDeliveryStore.getState().lastPullRequestPages,
        key,
      )?.items,
    ).toEqual([item]);
  });

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
      version: 2,
      excludedRegisteredRepoIds: ["repo-zeta"],
      pinnedRepositoryKeys: ["github.com/other-org/beta"],
    });
  });
});

describe("known delivery authors", () => {
  it("dedupes logins case-insensitively and keeps the freshest avatar", () => {
    const first = mergeKnownAuthors(
      [],
      [
        { login: "mara", avatarUrl: "https://avatars.test/mara" },
        { login: "devon" },
      ],
    );
    expect(first.map((author) => author.login)).toEqual(["mara", "devon"]);

    // A resighting under different casing is the same person: no duplicate
    // row, the sighting moves to the front, and its avatar fills the gap.
    const next = mergeKnownAuthors(first, [
      { login: "Devon", avatarUrl: "https://avatars.test/devon" },
    ]);
    expect(next.map((author) => author.login)).toEqual(["Devon", "mara"]);
    expect(next[0]!.avatarUrl).toBe("https://avatars.test/devon");
    // A later sighting without an avatar must not erase a known one.
    const kept = mergeKnownAuthors(next, [{ login: "devon" }]);
    expect(kept[0]!.avatarUrl).toBe("https://avatars.test/devon");
  });

  it("bounds the pool and drops the oldest sighting past the cap", () => {
    const crowd = Array.from({ length: 60 }, (_, index) => ({
      login: `login-${index}`,
    }));
    const merged = mergeKnownAuthors([], crowd);
    expect(merged).toHaveLength(50);
    expect(merged[0]!.login).toBe("login-0");
    expect(merged.some((author) => author.login === "login-59")).toBe(false);
  });

  it("harvests authors and actors from a completed poll and persists them", () => {
    const repo = repository("brightwave-inc", "alpha", "repo-alpha");
    useCodeDeliveryStore.getState().completeDeliveryPoll(
      [
        pullRequest(1, repo, {
          author: "mara",
          author_avatar_url: "https://avatars.test/mara",
        }),
      ],
      [run(11, repo, { actor: "dependabot[bot]" })],
      NOW,
    );

    expect(
      useCodeDeliveryStore.getState().knownAuthors.map((a) => a.login),
    ).toEqual(["mara", "dependabot[bot]"]);
    expect(
      JSON.parse(
        window.localStorage.getItem("tidebreak.code-delivery") ?? "{}",
      ),
    ).toMatchObject({
      knownAuthors: [
        { login: "mara", avatarUrl: "https://avatars.test/mara" },
        { login: "dependabot[bot]" },
      ],
    });
  });
});
