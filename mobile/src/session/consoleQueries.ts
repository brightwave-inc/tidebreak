/**
 * The console's reads and writes, as react-query options.
 *
 * `@tanstack/react-query` is already this app's data layer — the root layout
 * has mounted a `QueryClientProvider` since before this slice — so the ported
 * screens keep the shape they had in Tidewatch rather than being rewritten
 * against a second pattern.
 *
 * Every key opens with the active connection's id. Connections are plural
 * (#3408) and switching one must not show the previous gateway's cached
 * sandboxes for a frame; a scoped key makes that structurally impossible
 * instead of relying on an invalidation nobody forgets.
 *
 * Member reads self-narrow server-side, so nothing here asks for a fleet: a
 * member's sandbox list is their own, and an administrator's wider view is
 * slice #3402's problem, not this one's.
 */

import { queryOptions } from "@tanstack/react-query";
import type {
  CatalogResponse,
  ConversationView,
  CostLimitListResponse,
  SandboxConcurrencyResponse,
  SandboxDetailResponse,
  SandboxListResponse,
  SandboxMessageReceipt,
  SandboxState,
  SharedAppConsentResponse,
  SharedAppDetailResponse,
  SharedAppListResponse,
  SubscriptionListResponse,
  SubscriptionUsageResponse,
  UsageDimension,
  UsageResponse,
} from "../lib/consoleTypes";
import { isTerminal } from "../lib/consoleTypes";
import { hasErrorCode, HttpError } from "../lib/gatewayClient";
import type { GatewayIdentity } from "../lib/types";
import { connections } from "./runtime";
import {
  controlClient,
  controlPlaneClient,
  runtimeClient,
} from "./consoleClients";

/** The connection every key is scoped to, or a stable stand-in when unpaired. */
function consoleScopeKey(): string {
  return connections.active()?.id ?? "unpaired";
}

/** The code the API returns when this installation does not run sandboxes. */
export const SANDBOXES_NOT_ENABLED = "sandboxes_not_enabled";

/**
 * Whether an error is the installation saying it does not run sandboxes.
 *
 * Not a failure to render an error card for: it is a state with its own
 * explanation, and the screens answer it with a callout rather than dead
 * controls or a silently empty list.
 */
export function isSandboxesNotEnabled(error: unknown): boolean {
  return hasErrorCode(error, SANDBOXES_NOT_ENABLED);
}

const RETRY_NOT_4XX = (failureCount: number, error: unknown): boolean => {
  // A 401 must surface immediately and a 404 on a foreign resource will never
  // improve; only a transient server-side failure is worth asking again.
  if (error instanceof HttpError && error.status >= 400 && error.status < 500) {
    return false;
  }
  return failureCount < 2;
};

export const sandboxQueries = {
  /**
   * The caller's own sandboxes. `ownerId` is the signed-in account: the server
   * accepts a member naming themself, so passing it keeps the list "your runs"
   * even for an administrator whose unscoped read would be installation-wide.
   *
   * `states` narrows the fetch window rather than the render: it is a
   * comma-separated lifecycle set a gateway may be older than, and an older one
   * ignores unknown query parameters and answers unfiltered. Callers re-apply
   * `matchesStatusSelection` to what comes back.
   */
  list: (options: { ownerId?: string; states?: SandboxState[] | null } = {}) =>
    queryOptions({
      queryKey: [
        consoleScopeKey(),
        "sandboxes",
        options.ownerId ?? "all",
        options.states?.join(",") ?? "every-state",
      ],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<SandboxListResponse>(
          `/api/v1/admin/sandboxes?limit=100${
            options.ownerId
              ? `&user_id=${encodeURIComponent(options.ownerId)}`
              : ""
          }${
            options.states?.length
              ? `&states=${encodeURIComponent(options.states.join(","))}`
              : ""
          }`,
          { signal },
        ),
      // A disabled installation's refusal holds for the whole session —
      // enablement is a deployment value — so stop polling it.
      refetchInterval: (query) =>
        isSandboxesNotEnabled(query.state.error) ? false : 10_000,
      retry: RETRY_NOT_4XX,
    }),

  detail: (id: string) =>
    queryOptions({
      queryKey: [consoleScopeKey(), "sandbox", id],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<SandboxDetailResponse>(
          `/api/v1/admin/sandboxes/${encodeURIComponent(id)}`,
          { signal },
        ),
      // Poll while live, stop once terminal.
      refetchInterval: (query) => {
        const state = query.state.data?.sandbox.state;
        return state && isTerminal(state) ? false : 5_000;
      },
      retry: RETRY_NOT_4XX,
    }),

  concurrency: () =>
    queryOptions({
      queryKey: [consoleScopeKey(), "sandbox-concurrency"],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<SandboxConcurrencyResponse>(
          "/api/v1/admin/sandbox-concurrency",
          { signal },
        ),
      staleTime: 30_000,
      retry: (failureCount, error) =>
        isSandboxesNotEnabled(error) ? false : RETRY_NOT_4XX(failureCount, error),
    }),
};

export const sandboxMutations = {
  /**
   * Steering and the owner cancel ride the runtime surface's owner-scoped
   * verbs — no administrator role involved, so a member steers their own runs.
   * The administrator cancel is deliberately absent: it is a control-plane
   * write, and the admin console is slice #3402.
   */
  sendMessage: async (
    id: string,
    body: string,
    interrupt: boolean,
  ): Promise<SandboxMessageReceipt> => {
    const client = await runtimeClient();
    return client.request<SandboxMessageReceipt>(
      `/api/v1/runtime/sandboxes/${encodeURIComponent(id)}/messages`,
      { method: "POST", body: { body, interrupt } },
    );
  },

  cancel: async (id: string): Promise<unknown> => {
    const client = await runtimeClient();
    return client.request<unknown>(
      `/api/v1/runtime/sandboxes/${encodeURIComponent(id)}/cancel`,
      { method: "POST" },
    );
  },
};

/**
 * The signed-in account itself, from the `control` surface.
 *
 * Read here rather than taken from the connection record because this is what
 * decides ownership of the write affordances: the record's `identity` is a
 * cached display fact, and a screen that gates a destructive verb must ask the
 * credential who it is.
 */
export const meQueries = {
  identity: () =>
    queryOptions({
      queryKey: [consoleScopeKey(), "me"],
      queryFn: ({ signal }) =>
        controlClient().request<GatewayIdentity>("/api/v1/cli/me", { signal }),
      // A credential's identity never changes mid-session; switching
      // connections changes the scope key, so the next mount re-reads.
      staleTime: Infinity,
      retry: RETRY_NOT_4XX,
    }),
};

export const usageQueries = {
  /**
   * The unfiltered compatibility view: the `by_*` rollups, with a scope that
   * says whether this account's reads are self-narrowed or installation-wide.
   * Any filter or `group_by` switches the endpoint to query mode, so this one
   * deliberately sends no parameters.
   */
  summary: () =>
    queryOptions({
      queryKey: [consoleScopeKey(), "usage", "summary"],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<UsageResponse>("/api/v1/admin/usage", {
          signal,
        }),
      staleTime: 60_000,
      retry: RETRY_NOT_4XX,
    }),

  /**
   * Strictly one account's inference, cross-tabbed by one dimension. Query
   * mode: rows arrive in `grouped`. Used only when the caller's reads are
   * installation-wide and must be pinned back to themself.
   */
  mine: (userId: string, dimension: UsageDimension) =>
    queryOptions({
      queryKey: [consoleScopeKey(), "usage", "mine", userId, dimension],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<UsageResponse>(
          `/api/v1/admin/usage?user_id=${encodeURIComponent(userId)}&group_by=${dimension}`,
          { signal },
        ),
      staleTime: 60_000,
      retry: RETRY_NOT_4XX,
    }),
};

export const catalogQueries = {
  /** What this account may invoke, and how its connected apps stand. */
  mine: () =>
    queryOptions({
      queryKey: [consoleScopeKey(), "catalog"],
      queryFn: ({ signal }) =>
        controlClient().request<CatalogResponse>("/api/v1/me/catalog", {
          signal,
        }),
      staleTime: 300_000,
      retry: RETRY_NOT_4XX,
    }),
};

export const limitQueries = {
  /**
   * The caps that can refuse this account: its own, its teams', those on
   * models granted to it, and the installation-wide ones. Available to any
   * authenticated caller — asking why you are being refused is not an
   * administrator's question.
   */
  mine: () =>
    queryOptions({
      queryKey: [consoleScopeKey(), "cost-limits", "status"],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<CostLimitListResponse>(
          "/api/v1/admin/cost-limits/status",
          { signal },
        ),
      staleTime: 60_000,
      retry: RETRY_NOT_4XX,
    }),
};

export const subscriptionQueries = {
  /**
   * The caller's subscription-offering providers and every account they can
   * reach — their own, plus teammates' shared ones. `is_own` tells them apart,
   * and each binding carries the gateway's latest stored quota snapshot.
   */
  list: () =>
    queryOptions({
      queryKey: [consoleScopeKey(), "subscriptions"],
      queryFn: ({ signal }) =>
        controlClient().request<SubscriptionListResponse>(
          "/api/v1/cli/subscriptions",
          { signal },
        ),
      staleTime: 60_000,
      retry: RETRY_NOT_4XX,
    }),

  /**
   * A live account-usage re-read for one binding. The route is OpenAI-only and
   * owner-only by design — another provider kind answers 422, a borrowed
   * binding 404 — so callers enable this only where it can succeed and fall
   * back to the listing's snapshot everywhere else. Neither refusal is worth
   * retrying.
   */
  usage: (bindingId: string) =>
    queryOptions({
      queryKey: [consoleScopeKey(), "subscription-usage", bindingId],
      queryFn: ({ signal }) =>
        controlClient().request<SubscriptionUsageResponse>(
          `/api/v1/cli/subscriptions/${encodeURIComponent(bindingId)}/usage`,
          { signal },
        ),
      staleTime: 60_000,
      retry: false,
    }),
};

export const sharedAppQueries = {
  list: () =>
    queryOptions({
      queryKey: [consoleScopeKey(), "shared-apps"],
      queryFn: ({ signal }) =>
        controlClient().request<SharedAppListResponse>("/api/apps/shared", {
          signal,
        }),
      staleTime: 60_000,
      retry: RETRY_NOT_4XX,
    }),

  detail: (id: string) =>
    queryOptions({
      queryKey: [consoleScopeKey(), "shared-app", id],
      queryFn: ({ signal }) =>
        controlClient().request<SharedAppDetailResponse>(
          `/api/apps/shared/${encodeURIComponent(id)}`,
          { signal },
        ),
      retry: RETRY_NOT_4XX,
    }),
};

export const sharedAppMutations = {
  /**
   * Accept what one shared app may call as you. The revision is pinned: the
   * gateway refuses with `revision_moved` when the app has been revised since
   * the sheet the viewer read, rather than recording consent to a manifest they
   * never saw.
   */
  consent: (id: string, revisionId: string | undefined) =>
    controlClient().request<SharedAppConsentResponse>(
      `/api/apps/shared/${encodeURIComponent(id)}/consent`,
      { method: "POST", body: { revision_id: revisionId ?? null } },
    ),
};

export const conversationQueries = {
  detail: (id: string) =>
    queryOptions({
      queryKey: [consoleScopeKey(), "conversation", id],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<ConversationView>(
          `/api/v1/admin/conversations/${encodeURIComponent(id)}`,
          { signal },
        ),
      staleTime: 30_000,
      // A foreign or unknown id is a deliberate 404; retrying cannot change it.
      retry: false,
    }),
};
