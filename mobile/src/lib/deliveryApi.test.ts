import { describe, expect, it, vi } from "vitest";
import type { MachineClient } from "./machine";
import {
  groupMobileDeliveryPullRequests,
  listMobileDeliveryRepositories,
  mobileDeliveryCheckProgress,
  mobileDeliveryLaneCountLabel,
  mobileDeliveryLaneIsConfirmedEmpty,
  parseMobileDeliveryPullRequestsPage,
  parseMobileDeliveryRepositoriesSnapshot,
  queryMobileDeliveryPullRequests,
} from "./deliveryApi";

const repository = {
  host: "github.com",
  owner: "brightwave-inc",
  name: "tidebreak",
  name_with_owner: "brightwave-inc/tidebreak",
  url: "https://github.com/brightwave-inc/tidebreak",
  default_branch: "main",
  tidebreak_repo_id: "repo-1",
};

const capability = {
  found: true,
  authenticated: true,
  viewer_login: "naingthet",
  remediation: "",
};

const pullRequest = {
  id: "github.com/brightwave-inc/tidebreak#2852",
  repository,
  number: 2852,
  url: "https://github.com/brightwave-inc/tidebreak/pull/2852",
  title: "Browse and message chats",
  state: "open",
  draft: false,
  author: "naingthet",
  head_branch: "thet/mobile-chat-messaging",
  base_branch: "thet/mobile-spawn-flow",
  review_decision: "review_required",
  auto_merge_enabled: false,
  checks: [
    { name: "Mobile", bucket: "pass" },
    { name: "Release", bucket: "skipped", detail: "Not a release" },
  ],
  attention_reasons: [],
  ready_to_merge: false,
  workspace_links: [],
  labels: ["mobile"],
  created_at: "2026-08-27T20:00:00Z",
  updated_at: "2026-08-27T21:00:00Z",
};

const repositoriesSnapshot = {
  capability,
  repositories: [repository],
  errors: [],
  fetched_at: "2026-08-27T21:00:00Z",
};

const pullRequestsPage = {
  capability,
  items: [pullRequest],
  next_cursor: "page-2",
  errors: [
    {
      repository: {
        host: "github.com",
        owner: "brightwave-inc",
        name: "model-gateway",
      },
      kind: "rate_limited",
      message: "GitHub asked Tidebreak to retry later.",
      retry_at: "2026-08-27T21:05:00Z",
    },
  ],
  fetched_at: "2026-08-27T21:00:00Z",
};

function fakeClient(response: unknown): {
  client: Pick<MachineClient, "getJson" | "requestJson">;
  getJson: ReturnType<typeof vi.fn>;
  requestJson: ReturnType<typeof vi.fn>;
} {
  const getJson = vi.fn(async () => response);
  const requestJson = vi.fn(async () => response);
  return { client: { getJson, requestJson }, getJson, requestJson };
}

describe("mobile Delivery API contracts", () => {
  it("parses renderer-safe repository and capability fields", async () => {
    expect(parseMobileDeliveryRepositoriesSnapshot(repositoriesSnapshot)).toEqual({
      capability: { found: true, authenticated: true, remediation: "" },
      repositories: [
        {
          host: "github.com",
          owner: "brightwave-inc",
          name: "tidebreak",
          name_with_owner: "brightwave-inc/tidebreak",
          url: "https://github.com/brightwave-inc/tidebreak",
        },
      ],
      errors: [],
      fetched_at: "2026-08-27T21:00:00Z",
    });
    expect(
      parseMobileDeliveryRepositoriesSnapshot({
        ...repositoriesSnapshot,
        capability: { ...capability, authenticated: "yes" },
      }),
    ).toBeNull();
    expect(
      parseMobileDeliveryRepositoriesSnapshot({
        ...repositoriesSnapshot,
        repositories: [{ ...repository, url: "javascript:alert(1)" }],
      }),
    ).toBeNull();

    const listed = fakeClient(repositoriesSnapshot);
    await expect(listMobileDeliveryRepositories(listed.client)).resolves.toMatchObject({
      repositories: [expect.objectContaining({ name: "tidebreak" })],
    });
    expect(listed.getJson).toHaveBeenCalledWith(
      "/code/delivery/repositories",
    );

    const refreshed = fakeClient(repositoriesSnapshot);
    const signal = new AbortController().signal;
    await listMobileDeliveryRepositories(refreshed.client, {
      refresh: true,
      signal,
    });
    expect(refreshed.getJson).toHaveBeenCalledWith(
      "/code/delivery/repositories?refresh=true",
      { signal },
    );
  });

  it("sends an open pull-request query with bounded paging", async () => {
    const queried = fakeClient(pullRequestsPage);
    await expect(
      queryMobileDeliveryPullRequests(queried.client, {
        repositories: [
          { host: "github.com", owner: "brightwave-inc", name: "tidebreak" },
        ],
        cursor: "page-2",
        refresh: false,
      }),
    ).resolves.toMatchObject({ next_cursor: "page-2" });
    expect(queried.requestJson).toHaveBeenCalledWith(
      "/code/delivery/pull-requests/query",
      {
        method: "POST",
        body: {
          repositories: [
            {
              host: "github.com",
              owner: "brightwave-inc",
              name: "tidebreak",
            },
          ],
          states: ["open"],
          review_states: [],
          check_states: [],
          authors: [],
          attention_only: false,
          ready_only: false,
          cursor: "page-2",
          limit: 30,
          refresh: false,
        },
        expectedStatus: 200,
      },
    );

    await queryMobileDeliveryPullRequests(queried.client, {
      repositories: [
        { host: "github.com", owner: "brightwave-inc", name: "tidebreak" },
      ],
      refresh: true,
      signal: new AbortController().signal,
    });
    expect(queried.requestJson).toHaveBeenLastCalledWith(
      "/code/delivery/pull-requests/query",
      expect.objectContaining({
        signal: expect.any(AbortSignal),
        body: expect.objectContaining({
          refresh: true,
          limit: 30,
        }),
      }),
    );
    expect(
      queried.requestJson.mock.calls[1]?.[1]?.body,
    ).not.toHaveProperty("cursor");

    const invalidRefresh = fakeClient(pullRequestsPage);
    await expect(
      queryMobileDeliveryPullRequests(invalidRefresh.client, {
        repositories: [
          { host: "github.com", owner: "brightwave-inc", name: "tidebreak" },
        ],
        cursor: "page-2",
        refresh: true,
      }),
    ).rejects.toThrow(/cannot continue from an existing cursor/);
    expect(invalidRefresh.requestJson).not.toHaveBeenCalled();
  });

  it("rejects invalid pull-request fields without leaking unused fields", () => {
    expect(parseMobileDeliveryPullRequestsPage(pullRequestsPage)).toEqual({
      capability: { found: true, authenticated: true, remediation: "" },
      items: [
        {
          id: "github.com/brightwave-inc/tidebreak#2852",
          repository: {
            host: "github.com",
            owner: "brightwave-inc",
            name: "tidebreak",
            name_with_owner: "brightwave-inc/tidebreak",
            url: "https://github.com/brightwave-inc/tidebreak",
          },
          number: 2852,
          url: "https://github.com/brightwave-inc/tidebreak/pull/2852",
          title: "Browse and message chats",
          draft: false,
          author: "naingthet",
          head_branch: "thet/mobile-chat-messaging",
          base_branch: "thet/mobile-spawn-flow",
          review_decision: "review_required",
          checks: [
            { name: "Mobile", bucket: "pass" },
            { name: "Release", bucket: "skipped" },
          ],
          attention_reasons: [],
          ready_to_merge: false,
          updated_at: "2026-08-27T21:00:00Z",
        },
      ],
      next_cursor: "page-2",
      errors: [
        {
          repository: {
            host: "github.com",
            owner: "brightwave-inc",
            name: "model-gateway",
          },
          kind: "rate_limited",
          message: "GitHub asked Tidebreak to retry later.",
          retry_at: "2026-08-27T21:05:00Z",
        },
      ],
      fetched_at: "2026-08-27T21:00:00Z",
    });
    expect(
      parseMobileDeliveryPullRequestsPage({
        ...pullRequestsPage,
        items: [{ ...pullRequest, checks: [{ name: "Mobile", bucket: "later" }] }],
      }),
    ).toBeNull();
    expect(
      parseMobileDeliveryPullRequestsPage({
        ...pullRequestsPage,
        items: [{ ...pullRequest, number: 0 }],
      }),
    ).toBeNull();
  });

  it("groups lanes and counts skipped checks as terminal", () => {
    const parsed = parseMobileDeliveryPullRequestsPage({
      ...pullRequestsPage,
      next_cursor: undefined,
      items: [
        {
          ...pullRequest,
          id: "attention",
          attention_reasons: ["checks_failed"],
          ready_to_merge: false,
        },
        {
          ...pullRequest,
          id: "ready",
          attention_reasons: [],
          ready_to_merge: true,
        },
        {
          ...pullRequest,
          id: "progress",
          attention_reasons: [],
          ready_to_merge: false,
        },
      ],
    });
    expect(parsed).not.toBeNull();
    expect(groupMobileDeliveryPullRequests(parsed!.items)).toMatchObject({
      attention: [{ id: "attention" }],
      ready: [{ id: "ready" }],
      in_progress: [{ id: "progress" }],
    });
    expect(
      mobileDeliveryCheckProgress([
        { name: "Mobile", bucket: "pass" },
        { name: "Desktop", bucket: "fail" },
        { name: "Release", bucket: "skipped" },
      ]),
    ).toEqual({
      total: 3,
      terminal: 3,
      passing: 1,
      pending: 0,
      failing: 1,
      skipped: 1,
    });
    expect(
      mobileDeliveryCheckProgress([
        { name: "Mobile", bucket: "pass" },
        { name: "Desktop", bucket: "pending" },
        { name: "Release", bucket: "skipped" },
      ]),
    ).toMatchObject({ total: 3, terminal: 2, pending: 1, skipped: 1 });
    expect(mobileDeliveryLaneCountLabel(2, true)).toBe("2 loaded");
    expect(mobileDeliveryLaneCountLabel(2, false)).toBe("2");
    expect(mobileDeliveryLaneIsConfirmedEmpty([], true)).toBe(false);
    expect(mobileDeliveryLaneIsConfirmedEmpty([], false)).toBe(true);
    expect(
      mobileDeliveryLaneIsConfirmedEmpty(parsed!.items, false),
    ).toBe(false);
  });
});
