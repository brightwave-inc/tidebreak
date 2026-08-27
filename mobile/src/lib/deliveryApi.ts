import type {
  CodeDeliveryPrAttentionReason,
  PullRequestCheckBucket,
} from "../generated/wire";
import type { MachineClient } from "./machine";

type MachineJsonClient = Pick<MachineClient, "getJson" | "requestJson">;

export type MobileDeliveryCapability = {
  found: boolean;
  authenticated?: boolean;
  remediation: string;
};

export type MobileDeliveryRepositoryTarget = {
  host: string;
  owner: string;
  name: string;
};

export type MobileDeliveryRepository = MobileDeliveryRepositoryTarget & {
  name_with_owner: string;
  url: string;
};

export type MobileDeliverySourceError = {
  repository?: MobileDeliveryRepositoryTarget;
  kind: string;
  message: string;
  retry_at?: string;
};

export type MobileDeliveryCheck = {
  name: string;
  bucket: PullRequestCheckBucket;
};

export type MobileDeliveryPullRequest = {
  id: string;
  repository: MobileDeliveryRepository;
  number: number;
  url: string;
  title: string;
  draft: boolean;
  author?: string;
  head_branch: string;
  base_branch: string;
  review_decision?: string;
  checks: MobileDeliveryCheck[];
  attention_reasons: CodeDeliveryPrAttentionReason[];
  ready_to_merge: boolean;
  updated_at: string;
};

export type MobileDeliveryRepositoriesSnapshot = {
  capability: MobileDeliveryCapability;
  repositories: MobileDeliveryRepository[];
  errors: MobileDeliverySourceError[];
  fetched_at: string;
};

export type MobileDeliveryPullRequestsPage = {
  capability: MobileDeliveryCapability;
  items: MobileDeliveryPullRequest[];
  next_cursor?: string;
  errors: MobileDeliverySourceError[];
  fetched_at: string;
};

export type MobileDeliveryLane = "attention" | "ready" | "in_progress";

export type MobileDeliveryLanes = Record<
  MobileDeliveryLane,
  MobileDeliveryPullRequest[]
>;

export type MobileDeliveryCheckProgress = {
  total: number;
  terminal: number;
  passing: number;
  pending: number;
  failing: number;
  skipped: number;
};

const CHECK_BUCKETS = ["pass", "pending", "fail", "skipped"] as const;
const ATTENTION_REASONS = [
  "changes_requested",
  "checks_failed",
  "conflicts",
  "behind",
  "blocked",
] as const;

export const MOBILE_DELIVERY_PAGE_SIZE = 30;

function record(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object"
    ? (value as Record<string, unknown>)
    : null;
}

function nonEmpty(value: unknown): value is string {
  return typeof value === "string" && value.length > 0;
}

function optionalNonEmpty(value: unknown): value is string | undefined {
  return value === undefined || nonEmpty(value);
}

function timestamp(value: unknown): value is string {
  return nonEmpty(value) && !Number.isNaN(Date.parse(value));
}

function httpUrl(value: unknown): value is string {
  if (!nonEmpty(value)) return false;
  try {
    const url = new URL(value);
    return url.protocol === "https:" || url.protocol === "http:";
  } catch {
    return false;
  }
}

function positiveSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0;
}

function member<T extends string>(
  value: unknown,
  values: readonly T[],
): value is T {
  return typeof value === "string" && values.includes(value as T);
}

function parseCapability(value: unknown): MobileDeliveryCapability | null {
  const capability = record(value);
  if (
    !capability ||
    typeof capability.found !== "boolean" ||
    (capability.authenticated !== undefined &&
      typeof capability.authenticated !== "boolean") ||
    typeof capability.remediation !== "string"
  ) {
    return null;
  }
  return {
    found: capability.found,
    ...(capability.authenticated !== undefined
      ? { authenticated: capability.authenticated }
      : {}),
    remediation: capability.remediation,
  };
}

function parseRepositoryTarget(
  value: unknown,
): MobileDeliveryRepositoryTarget | null {
  const repository = record(value);
  if (
    !repository ||
    !nonEmpty(repository.host) ||
    !nonEmpty(repository.owner) ||
    !nonEmpty(repository.name)
  ) {
    return null;
  }
  return {
    host: repository.host,
    owner: repository.owner,
    name: repository.name,
  };
}

function parseRepository(value: unknown): MobileDeliveryRepository | null {
  const repository = record(value);
  const target = parseRepositoryTarget(repository);
  if (
    !repository ||
    !target ||
    !nonEmpty(repository.name_with_owner) ||
    !httpUrl(repository.url)
  ) {
    return null;
  }
  return {
    ...target,
    name_with_owner: repository.name_with_owner,
    url: repository.url,
  };
}

function parseSourceError(value: unknown): MobileDeliverySourceError | null {
  const error = record(value);
  if (
    !error ||
    !nonEmpty(error.kind) ||
    !nonEmpty(error.message) ||
    (error.retry_at !== undefined && !timestamp(error.retry_at))
  ) {
    return null;
  }
  const repository =
    error.repository === undefined
      ? undefined
      : parseRepositoryTarget(error.repository);
  if (error.repository !== undefined && !repository) return null;
  return {
    ...(repository ? { repository } : {}),
    kind: error.kind,
    message: error.message,
    ...(error.retry_at !== undefined ? { retry_at: error.retry_at } : {}),
  };
}

function parseCheck(value: unknown): MobileDeliveryCheck | null {
  const check = record(value);
  if (
    !check ||
    !nonEmpty(check.name) ||
    !member(check.bucket, CHECK_BUCKETS)
  ) {
    return null;
  }
  return { name: check.name, bucket: check.bucket };
}

function parsePullRequest(value: unknown): MobileDeliveryPullRequest | null {
  const pullRequest = record(value);
  if (
    !pullRequest ||
    !nonEmpty(pullRequest.id) ||
    !positiveSafeInteger(pullRequest.number) ||
    !httpUrl(pullRequest.url) ||
    !nonEmpty(pullRequest.title) ||
    typeof pullRequest.draft !== "boolean" ||
    !optionalNonEmpty(pullRequest.author) ||
    !nonEmpty(pullRequest.head_branch) ||
    !nonEmpty(pullRequest.base_branch) ||
    !optionalNonEmpty(pullRequest.review_decision) ||
    !Array.isArray(pullRequest.checks) ||
    !Array.isArray(pullRequest.attention_reasons) ||
    !pullRequest.attention_reasons.every((reason) =>
      member(reason, ATTENTION_REASONS),
    ) ||
    typeof pullRequest.ready_to_merge !== "boolean" ||
    !timestamp(pullRequest.updated_at)
  ) {
    return null;
  }
  const repository = parseRepository(pullRequest.repository);
  if (!repository) return null;
  const checks = pullRequest.checks.map(parseCheck);
  if (checks.some((check) => check === null)) return null;
  return {
    id: pullRequest.id,
    repository,
    number: pullRequest.number,
    url: pullRequest.url,
    title: pullRequest.title,
    draft: pullRequest.draft,
    ...(pullRequest.author !== undefined
      ? { author: pullRequest.author }
      : {}),
    head_branch: pullRequest.head_branch,
    base_branch: pullRequest.base_branch,
    ...(pullRequest.review_decision !== undefined
      ? { review_decision: pullRequest.review_decision }
      : {}),
    checks: checks as MobileDeliveryCheck[],
    attention_reasons:
      pullRequest.attention_reasons as CodeDeliveryPrAttentionReason[],
    ready_to_merge: pullRequest.ready_to_merge,
    updated_at: pullRequest.updated_at,
  };
}

function parseSourceErrors(value: unknown): MobileDeliverySourceError[] | null {
  if (!Array.isArray(value)) return null;
  const errors = value.map(parseSourceError);
  return errors.some((error) => error === null)
    ? null
    : (errors as MobileDeliverySourceError[]);
}

export function parseMobileDeliveryRepositoriesSnapshot(
  value: unknown,
): MobileDeliveryRepositoriesSnapshot | null {
  const snapshot = record(value);
  if (
    !snapshot ||
    !Array.isArray(snapshot.repositories) ||
    !timestamp(snapshot.fetched_at)
  ) {
    return null;
  }
  const capability = parseCapability(snapshot.capability);
  const repositories = snapshot.repositories.map(parseRepository);
  const errors = parseSourceErrors(snapshot.errors);
  if (
    !capability ||
    repositories.some((repository) => repository === null) ||
    !errors
  ) {
    return null;
  }
  return {
    capability,
    repositories: repositories as MobileDeliveryRepository[],
    errors,
    fetched_at: snapshot.fetched_at,
  };
}

export function parseMobileDeliveryPullRequestsPage(
  value: unknown,
): MobileDeliveryPullRequestsPage | null {
  const page = record(value);
  if (
    !page ||
    !Array.isArray(page.items) ||
    !optionalNonEmpty(page.next_cursor) ||
    !timestamp(page.fetched_at)
  ) {
    return null;
  }
  const capability = parseCapability(page.capability);
  const items = page.items.map(parsePullRequest);
  const errors = parseSourceErrors(page.errors);
  if (!capability || items.some((item) => item === null) || !errors) {
    return null;
  }
  return {
    capability,
    items: items as MobileDeliveryPullRequest[],
    ...(page.next_cursor !== undefined
      ? { next_cursor: page.next_cursor }
      : {}),
    errors,
    fetched_at: page.fetched_at,
  };
}

function required<T>(value: T | null, label: string): T {
  if (!value) throw new Error(`${label} response contains invalid data.`);
  return value;
}

export async function listMobileDeliveryRepositories(
  client: MachineJsonClient,
  options: {
    refresh?: boolean;
    signal?: AbortSignal;
  } = {},
): Promise<MobileDeliveryRepositoriesSnapshot> {
  const path = options.refresh
    ? "/code/delivery/repositories?refresh=true"
    : "/code/delivery/repositories";
  return required(
    parseMobileDeliveryRepositoriesSnapshot(
      options.signal
        ? await client.getJson(path, { signal: options.signal })
        : await client.getJson(path),
    ),
    "Delivery repositories",
  );
}

export async function queryMobileDeliveryPullRequests(
  client: MachineJsonClient,
  options: {
    repositories: MobileDeliveryRepositoryTarget[];
    cursor?: string;
    refresh?: boolean;
    limit?: number;
    signal?: AbortSignal;
  },
): Promise<MobileDeliveryPullRequestsPage> {
  if (options.refresh && options.cursor) {
    throw new Error(
      "Delivery refresh cannot continue from an existing cursor.",
    );
  }
  return required(
    parseMobileDeliveryPullRequestsPage(
      await client.requestJson("/code/delivery/pull-requests/query", {
        method: "POST",
        body: {
          repositories: options.repositories,
          states: ["open"],
          review_states: [],
          check_states: [],
          authors: [],
          attention_only: false,
          ready_only: false,
          ...(options.cursor ? { cursor: options.cursor } : {}),
          limit: options.limit ?? MOBILE_DELIVERY_PAGE_SIZE,
          refresh: options.refresh ?? false,
        },
        expectedStatus: 200,
        ...(options.signal ? { signal: options.signal } : {}),
      }),
    ),
    "Delivery pull requests",
  );
}

export function mobileDeliveryRepositoryTarget(
  repository: MobileDeliveryRepository,
): MobileDeliveryRepositoryTarget {
  return {
    host: repository.host,
    owner: repository.owner,
    name: repository.name,
  };
}

export function mobileDeliveryLane(
  pullRequest: MobileDeliveryPullRequest,
): MobileDeliveryLane {
  if (pullRequest.attention_reasons.length > 0) return "attention";
  if (pullRequest.ready_to_merge) return "ready";
  return "in_progress";
}

export function groupMobileDeliveryPullRequests(
  pullRequests: readonly MobileDeliveryPullRequest[],
): MobileDeliveryLanes {
  const lanes: MobileDeliveryLanes = {
    attention: [],
    ready: [],
    in_progress: [],
  };
  for (const pullRequest of pullRequests) {
    lanes[mobileDeliveryLane(pullRequest)].push(pullRequest);
  }
  return lanes;
}

export function mobileDeliveryLaneCountLabel(
  count: number,
  hasNextPage: boolean,
): string {
  return hasNextPage ? `${count} loaded` : String(count);
}

export function mobileDeliveryLaneIsConfirmedEmpty(
  pullRequests: readonly MobileDeliveryPullRequest[],
  hasNextPage: boolean,
): boolean {
  return pullRequests.length === 0 && !hasNextPage;
}

export function mobileDeliveryCheckProgress(
  checks: readonly MobileDeliveryCheck[],
): MobileDeliveryCheckProgress {
  const progress: MobileDeliveryCheckProgress = {
    total: checks.length,
    terminal: 0,
    passing: 0,
    pending: 0,
    failing: 0,
    skipped: 0,
  };
  for (const check of checks) {
    if (check.bucket === "pass") {
      progress.passing += 1;
      progress.terminal += 1;
    } else if (check.bucket === "pending") {
      progress.pending += 1;
    } else if (check.bucket === "fail") {
      progress.failing += 1;
      progress.terminal += 1;
    } else {
      progress.skipped += 1;
      progress.terminal += 1;
    }
  }
  return progress;
}

export function uniqueMobileDeliveryPullRequests(
  pullRequests: readonly MobileDeliveryPullRequest[],
): MobileDeliveryPullRequest[] {
  const seen = new Set<string>();
  return pullRequests.filter((pullRequest) => {
    if (seen.has(pullRequest.id)) return false;
    seen.add(pullRequest.id);
    return true;
  });
}

export function uniqueMobileDeliverySourceErrors(
  errors: readonly MobileDeliverySourceError[],
): MobileDeliverySourceError[] {
  const seen = new Set<string>();
  return errors.filter((error) => {
    const repository = error.repository
      ? `${error.repository.host}/${error.repository.owner}/${error.repository.name}`
      : "global";
    const key = `${repository}:${error.kind}:${error.message}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}
