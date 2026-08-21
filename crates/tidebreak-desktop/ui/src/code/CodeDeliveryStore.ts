import { create } from "zustand";

import type {
  CodeDeliveryPullRequestSummary,
  CodeDeliveryRunKind,
  CodeDeliveryRunSummary,
  CodeGitHubRepositoryRef,
  CodeGitHubRepositoryTarget,
} from "../api/types";

const STORAGE_KEY = "tidebreak.code-delivery";
const STORAGE_VERSION = 1;
const MAX_NOTIFICATIONS = 500;
const MAX_NOTIFICATION_AGE_MS = 30 * 24 * 60 * 60 * 1_000;

export type CodeDeliverySurface = "pull_requests" | "runs";

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

export const DEFAULT_DELIVERY_NOTIFICATION_RULES: CodeDeliveryNotificationRule[] =
  [
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

type PersistedCodeDeliveryState = {
  version: 1;
  manualRepositories: CodeGitHubRepositoryRef[];
  excludedRegisteredRepoIds: string[];
  pinnedRepositoryKeys: string[];
  savedViews: CodeDeliverySavedView[];
  notificationRules: CodeDeliveryNotificationRule[];
  notifications: CodeDeliveryNotification[];
  seenFingerprints: Record<string, string>;
  lastPollAt: string | null;
};

type CodeDeliveryStore = Omit<PersistedCodeDeliveryState, "version"> & {
  polling: boolean;
  monitorError: string | null;
  lastSuccessfulPollAt: string | null;
  rememberManualRepositories: (repositories: CodeGitHubRepositoryRef[]) => void;
  removeManualRepository: (key: string) => void;
  setRegisteredRepositoryExcluded: (repoId: string, excluded: boolean) => void;
  setRepositoryPinned: (key: string, pinned: boolean) => void;
  upsertSavedView: (view: CodeDeliverySavedView) => void;
  removeSavedView: (id: string) => void;
  updateNotificationRule: (
    id: CodeDeliveryNotificationRuleKind,
    patch: Partial<Omit<CodeDeliveryNotificationRule, "id">>,
  ) => void;
  ingestDeliveryPoll: (
    pullRequests: readonly CodeDeliveryPullRequestSummary[],
    runs: readonly CodeDeliveryRunSummary[],
    receivedAt?: string,
  ) => number;
  markNotificationRead: (id: string, read?: boolean) => void;
  markAllNotificationsRead: () => void;
  clearNotifications: () => void;
  setPollState: (polling: boolean, error?: string | null) => void;
  finishPoll: (at: string) => void;
  reset: () => void;
};

function emptyPersistedState(): PersistedCodeDeliveryState {
  return {
    version: STORAGE_VERSION,
    manualRepositories: [],
    excludedRegisteredRepoIds: [],
    pinnedRepositoryKeys: [],
    savedViews: [],
    notificationRules: DEFAULT_DELIVERY_NOTIFICATION_RULES.map((rule) => ({
      ...rule,
    })),
    notifications: [],
    seenFingerprints: {},
    lastPollAt: null,
  };
}

function readPersistedState(): PersistedCodeDeliveryState {
  if (typeof window === "undefined") return emptyPersistedState();
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return emptyPersistedState();
    return parsePersistedState(JSON.parse(raw)) ?? emptyPersistedState();
  } catch {
    return emptyPersistedState();
  }
}

function persist(state: CodeDeliveryStore): void {
  if (typeof window === "undefined") return;
  const payload: PersistedCodeDeliveryState = {
    version: STORAGE_VERSION,
    manualRepositories: state.manualRepositories,
    excludedRegisteredRepoIds: state.excludedRegisteredRepoIds,
    pinnedRepositoryKeys: state.pinnedRepositoryKeys,
    savedViews: state.savedViews,
    notificationRules: state.notificationRules,
    notifications: state.notifications,
    seenFingerprints: state.seenFingerprints,
    lastPollAt: state.lastPollAt,
  };
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(payload));
  } catch {
    // Delivery preferences and history are best-effort local state.
  }
}

const stored = readPersistedState();

export const useCodeDeliveryStore = create<CodeDeliveryStore>()((set, get) => ({
  ...stored,
  polling: false,
  monitorError: null,
  lastSuccessfulPollAt: stored.lastPollAt,
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
    persist(get());
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
    persist(get());
  },
  setRegisteredRepositoryExcluded: (repoId, excluded) => {
    const next = new Set(get().excludedRegisteredRepoIds);
    if (excluded) next.add(repoId);
    else next.delete(repoId);
    set({ excludedRegisteredRepoIds: [...next] });
    persist(get());
  },
  setRepositoryPinned: (key, pinned) => {
    const next = new Set(get().pinnedRepositoryKeys);
    if (pinned) next.add(key);
    else next.delete(key);
    set({ pinnedRepositoryKeys: [...next] });
    persist(get());
  },
  upsertSavedView: (view) => {
    set({
      savedViews: [
        view,
        ...get().savedViews.filter((candidate) => candidate.id !== view.id),
      ],
    });
    persist(get());
  },
  removeSavedView: (id) => {
    set({ savedViews: get().savedViews.filter((view) => view.id !== id) });
    persist(get());
  },
  updateNotificationRule: (id, patch) => {
    set({
      notificationRules: get().notificationRules.map((rule) =>
        rule.id === id ? { ...rule, ...patch, id } : rule,
      ),
    });
    persist(get());
  },
  ingestDeliveryPoll: (
    pullRequests,
    runs,
    receivedAt = new Date().toISOString(),
  ) => {
    const now = Date.parse(receivedAt);
    const cutoff = now - MAX_NOTIFICATION_AGE_MS;
    const seen = { ...get().seenFingerprints };
    const incoming: CodeDeliveryNotification[] = [];
    const rules = new Map(
      get().notificationRules.map((rule) => [rule.id, rule]),
    );

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
          rules.get("pull_request_attention"),
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
          rules.get("pull_request_ready"),
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
        run.status,
        run.conclusion ?? "",
      ].join(":");
      maybeAddRunNotification(
        incoming,
        seen,
        rules.get("run_failure"),
        run,
        fingerprint,
        receivedAt,
      );
    }

    const notifications = [...incoming, ...get().notifications]
      .filter((notification) => Date.parse(notification.occurredAt) >= cutoff)
      .sort((left, right) => right.occurredAt.localeCompare(left.occurredAt))
      .slice(0, MAX_NOTIFICATIONS);
    const retainedSeen = Object.fromEntries(
      Object.entries(seen).filter(([, at]) => Date.parse(at) >= cutoff),
    );
    set({ notifications, seenFingerprints: retainedSeen });
    persist(get());
    return incoming.length;
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
    persist(get());
  },
  markAllNotificationsRead: () => {
    const at = new Date().toISOString();
    set({
      notifications: get().notifications.map((notification) => ({
        ...notification,
        readAt: notification.readAt ?? at,
      })),
    });
    persist(get());
  },
  clearNotifications: () => {
    set({ notifications: [] });
    persist(get());
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
    persist(get());
  },
  reset: () => {
    const fresh = emptyPersistedState();
    set({
      ...fresh,
      polling: false,
      monitorError: null,
      lastSuccessfulPollAt: null,
    });
    persist(get());
  },
}));

function maybeAddNotification(
  incoming: CodeDeliveryNotification[],
  seen: Record<string, string>,
  rule: CodeDeliveryNotificationRule | undefined,
  pullRequest: CodeDeliveryPullRequestSummary,
  fingerprint: string,
  receivedAt: string,
  copy: {
    rule: "pull_request_attention" | "pull_request_ready";
    title: string;
    detail: string;
  },
): void {
  if (
    !ruleApplies(
      rule,
      pullRequest.repository,
      pullRequest.workspace_links.length > 0,
    )
  ) {
    return;
  }
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
  rule: CodeDeliveryNotificationRule | undefined,
  run: CodeDeliveryRunSummary,
  fingerprint: string,
  receivedAt: string,
): void {
  if (!ruleApplies(rule, run.repository, run.workspace_links.length > 0))
    return;
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

function ruleApplies(
  rule: CodeDeliveryNotificationRule | undefined,
  repository: CodeGitHubRepositoryRef,
  tidebreakLinked: boolean,
): boolean {
  if (!rule?.enabled) return false;
  if (rule.tidebreakLinkedOnly && !tidebreakLinked) return false;
  return (
    rule.repositoryKeys.length === 0 ||
    rule.repositoryKeys.includes(codeDeliveryRepositoryKey(repository))
  );
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

function parsePersistedState(
  value: unknown,
): PersistedCodeDeliveryState | null {
  if (!isRecord(value) || value.version !== STORAGE_VERSION) return null;
  const manualRepositories = parseRepositoryRefs(value.manualRepositories);
  const savedViews = parseSavedViews(value.savedViews);
  const notificationRules = parseNotificationRules(value.notificationRules);
  const notifications = parseNotifications(value.notifications);
  if (
    !manualRepositories ||
    !stringArray(value.excludedRegisteredRepoIds) ||
    !stringArray(value.pinnedRepositoryKeys) ||
    !savedViews ||
    !notificationRules ||
    !notifications ||
    !isStringRecord(value.seenFingerprints) ||
    !(value.lastPollAt === null || typeof value.lastPollAt === "string")
  ) {
    return null;
  }
  const byRule = new Map(notificationRules.map((rule) => [rule.id, rule]));
  for (const fallback of DEFAULT_DELIVERY_NOTIFICATION_RULES) {
    if (!byRule.has(fallback.id)) byRule.set(fallback.id, { ...fallback });
  }
  return {
    version: STORAGE_VERSION,
    manualRepositories,
    excludedRegisteredRepoIds: [...value.excludedRegisteredRepoIds],
    pinnedRepositoryKeys: [...value.pinnedRepositoryKeys],
    savedViews,
    notificationRules: [...byRule.values()],
    notifications,
    seenFingerprints: { ...value.seenFingerprints },
    lastPollAt: value.lastPollAt,
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

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function nonEmpty(value: unknown): value is string {
  return typeof value === "string" && value.length > 0;
}
