import { create } from "zustand";

import type { ApiClient } from "../api/client";
import { isRecord } from "../lib/guards";
import type {
  CodeDeliveryPullRequestSummary,
  CodeDeliveryRepositoriesSnapshot,
  CodeDeliveryRunKind,
  CodeDeliveryRunSummary,
  CodeDeliverySourceError,
  CodeGitHubRepositoryRef,
  CodeGitHubRepositoryTarget,
} from "../api/types";

const STORAGE_KEY = "tidebreak.code-delivery";
const STORAGE_VERSION = 1;
const MAX_NOTIFICATIONS = 500;
const MAX_KNOWN_AUTHORS = 50;
const MAX_SEEN_FINGERPRINTS = 5_000;
const MAX_NOTIFICATION_AGE_MS = 30 * 24 * 60 * 60 * 1_000;
const REPOSITORY_CACHE_MS = 2 * 60 * 1_000;
const MAX_PULL_REQUEST_PAGE_CACHE = 8;

export type CodeDeliverySurface = "pull_requests" | "runs";

/**
 * A login Delivery has seen on a pull request or run, kept so the author
 * filter can offer people instead of expecting logins typed from memory.
 * Recency-ordered and bounded; the newest sighting's avatar wins.
 */
export type CodeDeliveryAuthor = {
  login: string;
  avatarUrl?: string;
};

export type CodeDeliveryPrViewFilters = {
  search: string;
  repositoryKeys: string[];
  states: string[];
  reviewStates: string[];
  checkStates: string[];
  authors: string[];
  attentionOnly: boolean;
  readyOnly: boolean;
  tidebreakLinked?: boolean;
};

/** Last successful pull-request page for one query, kept for a fast first paint. */
export type CodeDeliveryPullRequestPageCache = {
  key: string;
  items: CodeDeliveryPullRequestSummary[];
  fetchedAt: string;
  nextCursor?: string;
  errors: CodeDeliverySourceError[];
};

export type CodeDeliveryRunViewFilters = {
  search: string;
  repositoryKeys: string[];
  kinds: CodeDeliveryRunKind[];
  statuses: string[];
  conclusions: string[];
  workflows: string[];
  environments: string[];
  branches: string[];
  events: string[];
  actors: string[];
  attentionOnly: boolean;
  tidebreakLinked?: boolean;
};

export type CodeDeliverySavedView =
  | {
      id: string;
      kind: "pull_requests";
      name: string;
      filters: CodeDeliveryPrViewFilters;
      createdAt: string;
    }
  | {
      id: string;
      kind: "runs";
      name: string;
      filters: CodeDeliveryRunViewFilters;
      createdAt: string;
    };

export type CodeDeliveryNotificationRuleKind =
  | "pull_request_attention"
  | "pull_request_ready"
  | "run_failure";

export type CodeDeliveryNotificationRule = {
  id: CodeDeliveryNotificationRuleKind;
  enabled: boolean;
  repositoryKeys: string[];
  tidebreakLinkedOnly: boolean;
};

export type CodeDeliveryNotificationTarget =
  | {
      kind: "pull_request";
      repository: CodeGitHubRepositoryTarget;
      number: number;
    }
  | {
      kind: "run";
      repository: CodeGitHubRepositoryTarget;
      runKind: CodeDeliveryRunKind;
      id: number;
    };

export type CodeDeliveryNotification = {
  id: string;
  fingerprint: string;
  rule: CodeDeliveryNotificationRuleKind;
  title: string;
  detail: string;
  repositoryName: string;
  occurredAt: string;
  receivedAt: string;
  readAt?: string;
  url: string;
  workspaceId?: string;
  target: CodeDeliveryNotificationTarget;
};

const LEGACY_DEFAULT_NOTIFICATION_RULES: CodeDeliveryNotificationRule[] = [
  {
    id: "pull_request_attention",
    enabled: true,
    repositoryKeys: [],
    tidebreakLinkedOnly: false,
  },
  {
    id: "pull_request_ready",
    enabled: true,
    repositoryKeys: [],
    tidebreakLinkedOnly: false,
  },
  {
    id: "run_failure",
    enabled: true,
    repositoryKeys: [],
    tidebreakLinkedOnly: false,
  },
];

type StoredCodeDeliveryState = {
  manualRepositories: CodeGitHubRepositoryRef[];
  excludedRegisteredRepoIds: string[];
  pinnedRepositoryKeys: string[];
  savedViews: CodeDeliverySavedView[];
  notifications: CodeDeliveryNotification[];
  seenFingerprints: Record<string, string>;
  lastPollAt: string | null;
  knownAuthors: CodeDeliveryAuthor[];
};

type PersistedCodeDeliveryState = StoredCodeDeliveryState & {
  version: 1;
  notificationRulesMigrated?: true;
  notificationRules?: CodeDeliveryNotificationRule[];
};

type HydratedCodeDeliveryState = StoredCodeDeliveryState & {
  /** Old rules stay here until every mapped server trigger is armed. */
  legacyNotificationRules: CodeDeliveryNotificationRule[] | null;
};

type CodeDeliveryStore = HydratedCodeDeliveryState & {
  polling: boolean;
  monitorError: string | null;
  lastSuccessfulPollAt: string | null;
  repositorySnapshot: CodeDeliveryRepositoriesSnapshot | null;
  repositoryLoading: boolean;
  repositoryError: string | null;
  repositoryFetchedAt: number | null;
  persistenceError: string | null;
  lastPullRequestPages: CodeDeliveryPullRequestPageCache[];
  rememberPullRequestPage: (page: CodeDeliveryPullRequestPageCache) => void;
  loadRepositories: (
    client: Pick<ApiClient, "getCodeDeliveryRepositories">,
    options?: { force?: boolean },
  ) => Promise<CodeDeliveryRepositoriesSnapshot>;
  rememberManualRepositories: (repositories: CodeGitHubRepositoryRef[]) => void;
  removeManualRepository: (key: string) => void;
  setRegisteredRepositoryExcluded: (repoId: string, excluded: boolean) => void;
  setRepositoryPinned: (key: string, pinned: boolean) => void;
  upsertSavedView: (view: CodeDeliverySavedView) => void;
  removeSavedView: (id: string) => void;
  completeNotificationRuleMigration: (
    rules: CodeDeliveryNotificationRule[],
  ) => void;
  ingestDeliveryPoll: (
    pullRequests: readonly CodeDeliveryPullRequestSummary[],
    runs: readonly CodeDeliveryRunSummary[],
    receivedAt?: string,
  ) => number;
  completeDeliveryPoll: (
    pullRequests: readonly CodeDeliveryPullRequestSummary[],
    runs: readonly CodeDeliveryRunSummary[],
    at: string,
  ) => number;
  rememberDeliveryAuthors: (authors: readonly CodeDeliveryAuthor[]) => void;
  markNotificationRead: (id: string, read?: boolean) => void;
  markAllNotificationsRead: () => void;
  clearNotifications: () => void;
  setPollState: (polling: boolean, error?: string | null) => void;
  finishPoll: (at: string) => void;
  reset: () => void;
};

function emptyPersistedState(): HydratedCodeDeliveryState {
  return {
    manualRepositories: [],
    excludedRegisteredRepoIds: [],
    pinnedRepositoryKeys: [],
    savedViews: [],
    notifications: [],
    seenFingerprints: {},
    lastPollAt: null,
    knownAuthors: [],
    legacyNotificationRules: null,
  };
}

function readPersistedState(): HydratedCodeDeliveryState {
  if (typeof window === "undefined") return emptyPersistedState();
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return emptyPersistedState();
    return parsePersistedState(JSON.parse(raw)) ?? emptyPersistedState();
  } catch {
    return emptyPersistedState();
  }
}

function persist(state: CodeDeliveryStore): string | null {
  if (typeof window === "undefined") return null;
  const payload: PersistedCodeDeliveryState = {
    version: STORAGE_VERSION,
    manualRepositories: state.manualRepositories,
    excludedRegisteredRepoIds: state.excludedRegisteredRepoIds,
    pinnedRepositoryKeys: state.pinnedRepositoryKeys,
    savedViews: state.savedViews,
    notifications: state.notifications,
    seenFingerprints: state.seenFingerprints,
    lastPollAt: state.lastPollAt,
    knownAuthors: state.knownAuthors,
    ...(state.legacyNotificationRules
      ? { notificationRules: state.legacyNotificationRules }
      : { notificationRulesMigrated: true }),
  };
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(payload));
    return null;
  } catch (error) {
    return error instanceof Error
      ? error.message
      : "Could not save delivery settings on this device.";
  }
}

const stored = readPersistedState();
let repositoryRequest: {
  client: Pick<ApiClient, "getCodeDeliveryRepositories">;
  promise: Promise<CodeDeliveryRepositoriesSnapshot>;
} | null = null;
let repositoryGeneration = 0;

export const useCodeDeliveryStore = create<CodeDeliveryStore>()((set, get) => {
  const persistCurrent = () => {
    const persistenceError = persist(get());
    if (get().persistenceError !== persistenceError) set({ persistenceError });
  };

  return {
    ...stored,
    polling: false,
    monitorError: null,
    lastSuccessfulPollAt: stored.lastPollAt,
    repositorySnapshot: null,
    repositoryLoading: false,
    repositoryError: null,
    repositoryFetchedAt: null,
    persistenceError: null,
    lastPullRequestPages: [],
    rememberPullRequestPage: (page) => {
      set({
        lastPullRequestPages: [
          page,
          ...get().lastPullRequestPages.filter(
            (candidate) => candidate.key !== page.key,
          ),
        ].slice(0, MAX_PULL_REQUEST_PAGE_CACHE),
      });
    },
    loadRepositories: async (client, options = {}) => {
      const current = get();
      if (
        !options.force &&
        current.repositorySnapshot &&
        current.repositoryFetchedAt !== null &&
        Date.now() - current.repositoryFetchedAt < REPOSITORY_CACHE_MS
      ) {
        return current.repositorySnapshot;
      }
      if (repositoryRequest?.client === client) {
        return repositoryRequest.promise;
      }

      const generation = ++repositoryGeneration;
      set({ repositoryLoading: true, repositoryError: null });
      const promise = client
        .getCodeDeliveryRepositories({ refreshAuth: options.force })
        .then((snapshot) => {
          if (generation === repositoryGeneration) {
            set({
              repositorySnapshot: snapshot,
              repositoryLoading: false,
              repositoryError: null,
              repositoryFetchedAt: Date.now(),
            });
          }
          return snapshot;
        })
        .catch((error: unknown) => {
          if (generation === repositoryGeneration) {
            set({
              repositoryLoading: false,
              repositoryError: deliveryErrorMessage(error),
            });
          }
          throw error;
        })
        .finally(() => {
          if (repositoryRequest?.promise === promise) repositoryRequest = null;
        });
      repositoryRequest = { client, promise };
      return promise;
    },
    rememberManualRepositories: (repositories) => {
      const byKey = new Map(
        get().manualRepositories.map((repository) => [
          codeDeliveryRepositoryKey(repository),
          repository,
        ]),
      );
      for (const repository of repositories) {
        byKey.set(codeDeliveryRepositoryKey(repository), repository);
      }
      set({ manualRepositories: [...byKey.values()] });
      persistCurrent();
    },
    removeManualRepository: (key) => {
      set({
        manualRepositories: get().manualRepositories.filter(
          (repository) => codeDeliveryRepositoryKey(repository) !== key,
        ),
        pinnedRepositoryKeys: get().pinnedRepositoryKeys.filter(
          (repositoryKey) => repositoryKey !== key,
        ),
      });
      persistCurrent();
    },
    setRegisteredRepositoryExcluded: (repoId, excluded) => {
      const next = new Set(get().excludedRegisteredRepoIds);
      if (excluded) next.add(repoId);
      else next.delete(repoId);
      set({ excludedRegisteredRepoIds: [...next] });
      persistCurrent();
    },
    setRepositoryPinned: (key, pinned) => {
      const next = new Set(get().pinnedRepositoryKeys);
      if (pinned) next.add(key);
      else next.delete(key);
      set({ pinnedRepositoryKeys: [...next] });
      persistCurrent();
    },
    upsertSavedView: (view) => {
      set({
        savedViews: [
          view,
          ...get().savedViews.filter((candidate) => candidate.id !== view.id),
        ],
      });
      persistCurrent();
    },
    removeSavedView: (id) => {
      set({ savedViews: get().savedViews.filter((view) => view.id !== id) });
      persistCurrent();
    },
    completeNotificationRuleMigration: (rules) => {
      if (get().legacyNotificationRules !== rules) return;
      set({ legacyNotificationRules: null });
      persistCurrent();
    },
    ingestDeliveryPoll: (
      pullRequests,
      runs,
      receivedAt = new Date().toISOString(),
    ) => {
      const next = buildDeliveryPoll(get(), pullRequests, runs, receivedAt);
      if (next.changed) {
        set({
          notifications: next.notifications,
          seenFingerprints: next.seenFingerprints,
        });
        persistCurrent();
      }
      return next.added;
    },
    completeDeliveryPoll: (pullRequests, runs, at) => {
      const next = buildDeliveryPoll(get(), pullRequests, runs, at);
      set({
        notifications: next.notifications,
        seenFingerprints: next.seenFingerprints,
        knownAuthors: mergeKnownAuthors(
          get().knownAuthors,
          deliveryAuthorSightings(pullRequests, runs),
        ),
        polling: false,
        monitorError: null,
        lastPollAt: at,
        lastSuccessfulPollAt: at,
      });
      persistCurrent();
      return next.added;
    },
    rememberDeliveryAuthors: (authors) => {
      const merged = mergeKnownAuthors(get().knownAuthors, authors);
      const current = get().knownAuthors;
      const unchanged =
        merged.length === current.length &&
        merged.every(
          (author, index) =>
            author.login === current[index]!.login &&
            author.avatarUrl === current[index]!.avatarUrl,
        );
      if (unchanged) return;
      set({ knownAuthors: merged });
      persistCurrent();
    },
    markNotificationRead: (id, read = true) => {
      const at = new Date().toISOString();
      set({
        notifications: get().notifications.map((notification) =>
          notification.id === id
            ? {
                ...notification,
                ...(read ? { readAt: at } : { readAt: undefined }),
              }
            : notification,
        ),
      });
      persistCurrent();
    },
    markAllNotificationsRead: () => {
      const at = new Date().toISOString();
      set({
        notifications: get().notifications.map((notification) => ({
          ...notification,
          readAt: notification.readAt ?? at,
        })),
      });
      persistCurrent();
    },
    clearNotifications: () => {
      set({ notifications: [] });
      persistCurrent();
    },
    setPollState: (polling, error = get().monitorError) => {
      set({ polling, monitorError: error });
    },
    finishPoll: (at) => {
      set({
        polling: false,
        monitorError: null,
        lastPollAt: at,
        lastSuccessfulPollAt: at,
      });
      persistCurrent();
    },
    reset: () => {
      repositoryGeneration += 1;
      repositoryRequest = null;
      const fresh = emptyPersistedState();
      set({
        ...fresh,
        polling: false,
        monitorError: null,
        lastSuccessfulPollAt: null,
        repositorySnapshot: null,
        repositoryLoading: false,
        repositoryError: null,
        repositoryFetchedAt: null,
        persistenceError: null,
        lastPullRequestPages: [],
      });
      persistCurrent();
    },
  };
});

/** Clear server-derived Delivery data without deleting reader preferences. */
export function resetCodeDeliveryHostState(): void {
  repositoryGeneration += 1;
  repositoryRequest = null;
  useCodeDeliveryStore.setState({
    polling: false,
    monitorError: null,
    lastSuccessfulPollAt: null,
    repositorySnapshot: null,
    repositoryLoading: false,
    repositoryError: null,
    repositoryFetchedAt: null,
    lastPullRequestPages: [],
  });
}

export function deliveryPullRequestPageKey(
  repositoryKeys: readonly string[],
  filters: CodeDeliveryPrViewFilters,
): string {
  return JSON.stringify({
    repositories: [...repositoryKeys].sort(),
    search: filters.search.trim(),
    states: [...filters.states].sort(),
    reviewStates: [...filters.reviewStates].sort(),
    checkStates: [...filters.checkStates].sort(),
    authors: [...filters.authors].sort(),
    attentionOnly: filters.attentionOnly,
    readyOnly: filters.readyOnly,
    tidebreakLinked: filters.tidebreakLinked ?? false,
  });
}

export function rememberedPullRequestPage(
  pages: readonly CodeDeliveryPullRequestPageCache[],
  key: string,
): CodeDeliveryPullRequestPageCache | undefined {
  return pages.find((page) => page.key === key);
}

function buildDeliveryPoll(
  state: Pick<CodeDeliveryStore, "notifications" | "seenFingerprints">,
  pullRequests: readonly CodeDeliveryPullRequestSummary[],
  runs: readonly CodeDeliveryRunSummary[],
  receivedAt: string,
): {
  notifications: CodeDeliveryNotification[];
  seenFingerprints: Record<string, string>;
  added: number;
  changed: boolean;
} {
  const now = Date.parse(receivedAt);
  const cutoff = now - MAX_NOTIFICATION_AGE_MS;
  const seen = { ...state.seenFingerprints };
  const incoming: CodeDeliveryNotification[] = [];

  for (const pullRequest of pullRequests) {
    if (Date.parse(pullRequest.updated_at) < cutoff) continue;
    if (pullRequest.attention_reasons.length > 0) {
      const fingerprint = [
        "pr-attention",
        pullRequest.id,
        pullRequest.head_sha ?? "",
        [...pullRequest.attention_reasons].sort().join(","),
      ].join(":");
      maybeAddNotification(
        incoming,
        seen,
        pullRequest,
        fingerprint,
        receivedAt,
        {
          rule: "pull_request_attention",
          title: `${pullRequest.repository.name_with_owner} #${pullRequest.number} needs attention`,
          detail: pullRequest.title,
        },
      );
    }
    if (pullRequest.ready_to_merge) {
      const fingerprint = [
        "pr-ready",
        pullRequest.id,
        pullRequest.head_sha ?? "",
      ].join(":");
      maybeAddNotification(
        incoming,
        seen,
        pullRequest,
        fingerprint,
        receivedAt,
        {
          rule: "pull_request_ready",
          title: `${pullRequest.repository.name_with_owner} #${pullRequest.number} is ready`,
          detail: pullRequest.title,
        },
      );
    }
  }

  for (const run of runs) {
    if (
      Date.parse(run.updated_at) < cutoff ||
      run.attention_reasons.length === 0
    ) {
      continue;
    }
    const fingerprint = [
      "run-failure",
      run.id,
      run.run_attempt ?? run.updated_at,
      run.status,
      run.conclusion ?? "",
    ].join(":");
    maybeAddRunNotification(incoming, seen, run, fingerprint, receivedAt);
  }

  const notifications = [...incoming, ...state.notifications]
    .filter((notification) => Date.parse(notification.occurredAt) >= cutoff)
    .sort((left, right) => right.occurredAt.localeCompare(left.occurredAt))
    .slice(0, MAX_NOTIFICATIONS);
  const seenFingerprints = Object.fromEntries(
    Object.entries(seen)
      .filter(([, at]) => Date.parse(at) >= cutoff)
      .sort((left, right) => right[1].localeCompare(left[1]))
      .slice(0, MAX_SEEN_FINGERPRINTS),
  );
  return {
    notifications,
    seenFingerprints,
    added: incoming.length,
    changed:
      !sameNotifications(state.notifications, notifications) ||
      !sameStringRecord(state.seenFingerprints, seenFingerprints),
  };
}

function sameNotifications(
  left: readonly CodeDeliveryNotification[],
  right: readonly CodeDeliveryNotification[],
): boolean {
  return (
    left.length === right.length &&
    left.every(
      (notification, index) =>
        notification.id === right[index]?.id &&
        notification.readAt === right[index]?.readAt,
    )
  );
}

function sameStringRecord(
  left: Record<string, string>,
  right: Record<string, string>,
): boolean {
  const entries = Object.entries(left);
  return (
    entries.length === Object.keys(right).length &&
    entries.every(([key, value]) => right[key] === value)
  );
}

function deliveryErrorMessage(error: unknown): string {
  return error instanceof Error
    ? error.message
    : "Could not load GitHub repositories.";
}

function maybeAddNotification(
  incoming: CodeDeliveryNotification[],
  seen: Record<string, string>,
  pullRequest: CodeDeliveryPullRequestSummary,
  fingerprint: string,
  receivedAt: string,
  copy: {
    rule: "pull_request_attention" | "pull_request_ready";
    title: string;
    detail: string;
  },
): void {
  if (seen[fingerprint]) return;
  seen[fingerprint] = receivedAt;
  incoming.push({
    id: fingerprint,
    fingerprint,
    rule: copy.rule,
    title: copy.title,
    detail: copy.detail,
    repositoryName: pullRequest.repository.name_with_owner,
    occurredAt: pullRequest.updated_at,
    receivedAt,
    url: pullRequest.url,
    ...(pullRequest.workspace_links[0]
      ? { workspaceId: pullRequest.workspace_links[0].workspace_id }
      : {}),
    target: {
      kind: "pull_request",
      repository: codeDeliveryRepositoryTarget(pullRequest.repository),
      number: pullRequest.number,
    },
  });
}

function maybeAddRunNotification(
  incoming: CodeDeliveryNotification[],
  seen: Record<string, string>,
  run: CodeDeliveryRunSummary,
  fingerprint: string,
  receivedAt: string,
): void {
  if (seen[fingerprint]) return;
  seen[fingerprint] = receivedAt;
  incoming.push({
    id: fingerprint,
    fingerprint,
    rule: "run_failure",
    title: `${run.repository.name_with_owner} ${run.name} failed`,
    detail: run.conclusion ?? run.status,
    repositoryName: run.repository.name_with_owner,
    occurredAt: run.updated_at,
    receivedAt,
    url: run.url,
    ...(run.workspace_links[0]
      ? { workspaceId: run.workspace_links[0].workspace_id }
      : {}),
    target: {
      kind: "run",
      repository: codeDeliveryRepositoryTarget(run.repository),
      runKind: run.kind,
      id: run.github_id,
    },
  });
}

export function codeDeliveryRepositoryKey(
  repository: CodeGitHubRepositoryRef | CodeGitHubRepositoryTarget,
): string {
  return `${repository.host}/${repository.owner}/${repository.name}`.toLocaleLowerCase();
}

export function codeDeliveryRepositoryTarget(
  repository: CodeGitHubRepositoryRef,
): CodeGitHubRepositoryTarget {
  return {
    host: repository.host,
    owner: repository.owner,
    name: repository.name,
  };
}

export function trackedCodeDeliveryRepositories(
  discovered: readonly CodeGitHubRepositoryRef[],
  state: Pick<
    CodeDeliveryStore,
    "manualRepositories" | "excludedRegisteredRepoIds" | "pinnedRepositoryKeys"
  >,
): CodeGitHubRepositoryRef[] {
  const repositories = new Map<string, CodeGitHubRepositoryRef>();
  for (const repository of discovered) {
    if (
      repository.tidebreak_repo_id &&
      state.excludedRegisteredRepoIds.includes(repository.tidebreak_repo_id)
    ) {
      continue;
    }
    repositories.set(codeDeliveryRepositoryKey(repository), repository);
  }
  for (const repository of state.manualRepositories) {
    const key = codeDeliveryRepositoryKey(repository);
    if (!repositories.has(key)) repositories.set(key, repository);
  }
  const pinned = new Set(state.pinnedRepositoryKeys);
  return [...repositories.values()].sort((left, right) => {
    const leftPinned = pinned.has(codeDeliveryRepositoryKey(left));
    const rightPinned = pinned.has(codeDeliveryRepositoryKey(right));
    if (leftPinned !== rightPinned) return leftPinned ? -1 : 1;
    return left.name_with_owner.localeCompare(
      right.name_with_owner,
      undefined,
      {
        sensitivity: "base",
      },
    );
  });
}

export function unreadCodeDeliveryNotifications(
  state: Pick<CodeDeliveryStore, "notifications">,
): number {
  return state.notifications.reduce(
    (count, notification) => count + (notification.readAt ? 0 : 1),
    0,
  );
}

/**
 * Fold new sightings into the known-author pool: dedupe logins
 * case-insensitively, move a resighted login to the front, let a sighting
 * that carries an avatar refresh a stale one, and drop the oldest past the
 * cap. Returns the current array unchanged when nothing moved, so callers
 * can skip a persist.
 */
export function mergeKnownAuthors(
  current: readonly CodeDeliveryAuthor[],
  sightings: readonly CodeDeliveryAuthor[],
): CodeDeliveryAuthor[] {
  const incoming = new Map<string, CodeDeliveryAuthor>();
  for (const sighting of sightings) {
    const login = sighting.login.trim();
    if (!login) continue;
    const key = login.toLowerCase();
    const previous = incoming.get(key);
    incoming.set(key, {
      login,
      ...(sighting.avatarUrl || previous?.avatarUrl
        ? { avatarUrl: sighting.avatarUrl ?? previous?.avatarUrl }
        : {}),
    });
  }
  if (incoming.size === 0) return [...current];
  const merged: CodeDeliveryAuthor[] = [];
  for (const [key, sighting] of incoming) {
    const known = current.find((author) => author.login.toLowerCase() === key);
    const avatarUrl = sighting.avatarUrl ?? known?.avatarUrl;
    merged.push({ login: sighting.login, ...(avatarUrl ? { avatarUrl } : {}) });
  }
  for (const author of current) {
    if (!incoming.has(author.login.toLowerCase())) merged.push(author);
  }
  const bounded = merged.slice(0, MAX_KNOWN_AUTHORS);
  const unchanged =
    bounded.length === current.length &&
    bounded.every(
      (author, index) =>
        author.login === current[index]!.login &&
        author.avatarUrl === current[index]!.avatarUrl,
    );
  return unchanged ? [...current] : bounded;
}

/** The logins one page of delivery rows contributes to the author pool. */
export function deliveryAuthorSightings(
  pullRequests: readonly CodeDeliveryPullRequestSummary[],
  runs: readonly CodeDeliveryRunSummary[],
): CodeDeliveryAuthor[] {
  return [
    ...pullRequests.flatMap((item) =>
      item.author
        ? [
            {
              login: item.author,
              ...(item.author_avatar_url
                ? { avatarUrl: item.author_avatar_url }
                : {}),
            },
          ]
        : [],
    ),
    ...runs.flatMap((item) => (item.actor ? [{ login: item.actor }] : [])),
  ];
}

/** Tolerant: a blob written before authors existed hydrates to an empty pool. */
function parseKnownAuthors(value: unknown): CodeDeliveryAuthor[] {
  if (!Array.isArray(value)) return [];
  const authors: CodeDeliveryAuthor[] = [];
  for (const entry of value) {
    if (!isRecord(entry) || typeof entry.login !== "string") continue;
    const login = entry.login.trim();
    if (!login) continue;
    authors.push({
      login,
      ...(typeof entry.avatarUrl === "string" && entry.avatarUrl
        ? { avatarUrl: entry.avatarUrl }
        : {}),
    });
  }
  return authors.slice(0, MAX_KNOWN_AUTHORS);
}

function parsePersistedState(value: unknown): HydratedCodeDeliveryState | null {
  if (!isRecord(value) || value.version !== STORAGE_VERSION) return null;
  const manualRepositories = parseRepositoryRefs(value.manualRepositories);
  const savedViews = parseSavedViews(value.savedViews);
  const notifications = parseNotifications(value.notifications);
  const rulesMigrated = value.notificationRulesMigrated === true;
  const notificationRules = rulesMigrated
    ? null
    : parseNotificationRules(value.notificationRules);
  if (
    !manualRepositories ||
    !stringArray(value.excludedRegisteredRepoIds) ||
    !stringArray(value.pinnedRepositoryKeys) ||
    !savedViews ||
    !(
      value.notificationRulesMigrated === undefined ||
      value.notificationRulesMigrated === true
    ) ||
    (!rulesMigrated && !notificationRules) ||
    !notifications ||
    !isStringRecord(value.seenFingerprints) ||
    !(value.lastPollAt === null || typeof value.lastPollAt === "string")
  ) {
    return null;
  }
  const byRule = new Map(
    (notificationRules ?? []).map((rule) => [rule.id, rule]),
  );
  if (!rulesMigrated) {
    for (const fallback of LEGACY_DEFAULT_NOTIFICATION_RULES) {
      if (!byRule.has(fallback.id)) byRule.set(fallback.id, { ...fallback });
    }
  }
  return {
    manualRepositories,
    excludedRegisteredRepoIds: [...value.excludedRegisteredRepoIds],
    pinnedRepositoryKeys: [...value.pinnedRepositoryKeys],
    savedViews,
    notifications,
    seenFingerprints: { ...value.seenFingerprints },
    lastPollAt: value.lastPollAt,
    knownAuthors: parseKnownAuthors(value.knownAuthors),
    legacyNotificationRules: rulesMigrated ? null : [...byRule.values()],
  };
}

function parseRepositoryRefs(value: unknown): CodeGitHubRepositoryRef[] | null {
  if (!Array.isArray(value)) return null;
  const parsed: CodeGitHubRepositoryRef[] = [];
  for (const item of value) {
    if (
      !isRecord(item) ||
      !nonEmpty(item.host) ||
      !nonEmpty(item.owner) ||
      !nonEmpty(item.name) ||
      !nonEmpty(item.name_with_owner) ||
      !nonEmpty(item.url) ||
      !(
        item.default_branch === undefined ||
        typeof item.default_branch === "string"
      ) ||
      !(
        item.tidebreak_repo_id === undefined ||
        typeof item.tidebreak_repo_id === "string"
      )
    ) {
      return null;
    }
    parsed.push({
      host: item.host,
      owner: item.owner,
      name: item.name,
      name_with_owner: item.name_with_owner,
      url: item.url,
      ...(item.default_branch !== undefined
        ? { default_branch: item.default_branch }
        : {}),
      ...(item.tidebreak_repo_id !== undefined
        ? { tidebreak_repo_id: item.tidebreak_repo_id }
        : {}),
    });
  }
  return parsed;
}

function parseSavedViews(value: unknown): CodeDeliverySavedView[] | null {
  if (!Array.isArray(value)) return null;
  const views: CodeDeliverySavedView[] = [];
  for (const item of value) {
    if (
      !isRecord(item) ||
      !nonEmpty(item.id) ||
      !nonEmpty(item.name) ||
      !nonEmpty(item.createdAt) ||
      !isRecord(item.filters)
    ) {
      return null;
    }
    if (item.kind === "pull_requests") {
      const filters = parsePrFilters(item.filters);
      if (!filters) return null;
      views.push({
        id: item.id,
        kind: item.kind,
        name: item.name,
        createdAt: item.createdAt,
        filters,
      });
    } else if (item.kind === "runs") {
      const filters = parseRunFilters(item.filters);
      if (!filters) return null;
      views.push({
        id: item.id,
        kind: item.kind,
        name: item.name,
        createdAt: item.createdAt,
        filters,
      });
    } else {
      return null;
    }
  }
  return views;
}

function parsePrFilters(
  value: Record<string, unknown>,
): CodeDeliveryPrViewFilters | null {
  if (
    typeof value.search !== "string" ||
    !stringArray(value.repositoryKeys) ||
    !stringArray(value.states) ||
    !stringArray(value.reviewStates) ||
    !stringArray(value.checkStates) ||
    !stringArray(value.authors) ||
    typeof value.attentionOnly !== "boolean" ||
    typeof value.readyOnly !== "boolean" ||
    !(
      value.tidebreakLinked === undefined ||
      typeof value.tidebreakLinked === "boolean"
    )
  ) {
    return null;
  }
  return {
    search: value.search,
    repositoryKeys: [...value.repositoryKeys],
    states: [...value.states],
    reviewStates: [...value.reviewStates],
    checkStates: [...value.checkStates],
    authors: [...value.authors],
    attentionOnly: value.attentionOnly,
    readyOnly: value.readyOnly,
    ...(value.tidebreakLinked !== undefined
      ? { tidebreakLinked: value.tidebreakLinked }
      : {}),
  };
}

function parseRunFilters(
  value: Record<string, unknown>,
): CodeDeliveryRunViewFilters | null {
  if (
    typeof value.search !== "string" ||
    !stringArray(value.repositoryKeys) ||
    !Array.isArray(value.kinds) ||
    !value.kinds.every(
      (kind) => kind === "workflow_run" || kind === "deployment",
    ) ||
    !stringArray(value.statuses) ||
    !stringArray(value.conclusions) ||
    !stringArray(value.workflows) ||
    !stringArray(value.environments) ||
    !stringArray(value.branches) ||
    !stringArray(value.events) ||
    !stringArray(value.actors) ||
    typeof value.attentionOnly !== "boolean" ||
    !(
      value.tidebreakLinked === undefined ||
      typeof value.tidebreakLinked === "boolean"
    )
  ) {
    return null;
  }
  return {
    search: value.search,
    repositoryKeys: [...value.repositoryKeys],
    kinds: [...value.kinds] as CodeDeliveryRunKind[],
    statuses: [...value.statuses],
    conclusions: [...value.conclusions],
    workflows: [...value.workflows],
    environments: [...value.environments],
    branches: [...value.branches],
    events: [...value.events],
    actors: [...value.actors],
    attentionOnly: value.attentionOnly,
    ...(value.tidebreakLinked !== undefined
      ? { tidebreakLinked: value.tidebreakLinked }
      : {}),
  };
}

function parseNotificationRules(
  value: unknown,
): CodeDeliveryNotificationRule[] | null {
  if (!Array.isArray(value)) return null;
  const rules: CodeDeliveryNotificationRule[] = [];
  for (const item of value) {
    if (
      !isRecord(item) ||
      !isRuleKind(item.id) ||
      typeof item.enabled !== "boolean" ||
      !stringArray(item.repositoryKeys) ||
      typeof item.tidebreakLinkedOnly !== "boolean"
    ) {
      return null;
    }
    rules.push({
      id: item.id,
      enabled: item.enabled,
      repositoryKeys: [...item.repositoryKeys],
      tidebreakLinkedOnly: item.tidebreakLinkedOnly,
    });
  }
  return rules;
}

function parseNotifications(value: unknown): CodeDeliveryNotification[] | null {
  if (!Array.isArray(value)) return null;
  const notifications: CodeDeliveryNotification[] = [];
  for (const item of value) {
    if (
      !isRecord(item) ||
      !nonEmpty(item.id) ||
      !nonEmpty(item.fingerprint) ||
      !isRuleKind(item.rule) ||
      !nonEmpty(item.title) ||
      typeof item.detail !== "string" ||
      !nonEmpty(item.repositoryName) ||
      !nonEmpty(item.occurredAt) ||
      !nonEmpty(item.receivedAt) ||
      !(item.readAt === undefined || typeof item.readAt === "string") ||
      !nonEmpty(item.url) ||
      !(
        item.workspaceId === undefined || typeof item.workspaceId === "string"
      ) ||
      !isRecord(item.target)
    ) {
      return null;
    }
    const target = parseNotificationTarget(item.target);
    if (!target) return null;
    notifications.push({
      id: item.id,
      fingerprint: item.fingerprint,
      rule: item.rule,
      title: item.title,
      detail: item.detail,
      repositoryName: item.repositoryName,
      occurredAt: item.occurredAt,
      receivedAt: item.receivedAt,
      url: item.url,
      target,
      ...(item.readAt !== undefined ? { readAt: item.readAt } : {}),
      ...(item.workspaceId !== undefined
        ? { workspaceId: item.workspaceId }
        : {}),
    });
  }
  return notifications;
}

function parseNotificationTarget(
  value: Record<string, unknown>,
): CodeDeliveryNotificationTarget | null {
  if (!isRecord(value.repository)) return null;
  const repository = value.repository;
  if (
    !nonEmpty(repository.host) ||
    !nonEmpty(repository.owner) ||
    !nonEmpty(repository.name)
  ) {
    return null;
  }
  const targetRepository = {
    host: repository.host,
    owner: repository.owner,
    name: repository.name,
  };
  if (value.kind === "pull_request" && Number.isSafeInteger(value.number)) {
    return {
      kind: "pull_request",
      repository: targetRepository,
      number: value.number as number,
    };
  }
  if (
    value.kind === "run" &&
    (value.runKind === "workflow_run" || value.runKind === "deployment") &&
    Number.isSafeInteger(value.id)
  ) {
    return {
      kind: "run",
      repository: targetRepository,
      runKind: value.runKind,
      id: value.id as number,
    };
  }
  return null;
}

function isRuleKind(value: unknown): value is CodeDeliveryNotificationRuleKind {
  return (
    value === "pull_request_attention" ||
    value === "pull_request_ready" ||
    value === "run_failure"
  );
}

function stringArray(value: unknown): value is string[] {
  return (
    Array.isArray(value) && value.every((item) => typeof item === "string")
  );
}

function isStringRecord(value: unknown): value is Record<string, string> {
  return (
    isRecord(value) &&
    Object.values(value).every((item) => typeof item === "string")
  );
}

function nonEmpty(value: unknown): value is string {
  return typeof value === "string" && value.length > 0;
}
