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
 * Member reads self-narrow server-side, so the member half is correct for both
 * roles: a member's sandbox list is simply their own. The administrator reads
 * at the bottom are the ones the gateway refuses outright for a member, and
 * they are gated on the connection's cached role before they are enabled.
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
import { AUDIT_PAGE_SIZE, PEOPLE_PAGE_SIZE } from "../lib/admin";
import { consoleCacheScope } from "../lib/connections";
import type {
  AuditEventPage,
  AuthenticationPolicy,
  ConnectedAppPage,
  CostControlSettings,
  CostLimitPage,
  GuardrailActivityPage,
  GuardrailPolicyPage,
  IdentityProviderPage,
  InstallationUsage,
  McpEndpointPage,
  ModelProviderPage,
  PersonPage,
  ProviderModelPage,
  ScimConnectorPage,
  TeamPage,
} from "../lib/gatewayAdmin";
import { hasErrorCode, HttpError } from "../lib/gatewayClient";
import type { GatewayIdentity } from "../lib/types";
import { connections } from "./runtime";
import {
  controlClient,
  controlPlaneClient,
  runtimeClient,
} from "./consoleClients";

/**
 * The connection every key is scoped to, or a stable stand-in when unpaired.
 *
 * `consoleCacheScope`, not the connection id: the id names the deployment and
 * is reused when somebody else signs in to it, and these caches must never
 * outlive the sign-in that filled them. See the note there — the
 * administration surfaces are derived from a cached usage read, so a stale
 * entry is not merely stale, it answers a question about who you are.
 */
function consoleScopeKey(): string {
  return consoleCacheScope(connections.active());
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
   * `adminCancel` is the one administrator verb here, and the only in-app
   * control-plane write in the app.
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

  /**
   * The administrator cancel. Unlike the owner verb above it is not
   * ownership-scoped: the administrator role, re-read live on every request,
   * lets it stop any run in the installation. It rides the same control-plane
   * surface as the administrator reads, and the gateway accepts it only from a
   * session whose consent granted `control_plane:write` (mg ADR 0102) — which
   * is what `canAdminCancel` gates the button on.
   *
   * The reason is sent so the sandbox's own record reads as what happened
   * rather than as a run somebody killed; the gateway records it as the
   * termination detail.
   */
  adminCancel: (id: string): Promise<unknown> =>
    controlPlaneClient().request<unknown>(
      `/api/v1/admin/sandboxes/${encodeURIComponent(id)}/cancel`,
      { method: "POST", body: { reason: "cancelled from Tidebreak mobile" } },
    ),
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
   *
   * One read serves three callers — the hub's stat grid, the member activity
   * screen, and the administrator usage screen — so the response type carries
   * the installation-wide rollups too, taken from the generated document
   * (`InstallationUsage`) rather than transcribed again. They are declared
   * optional because a member's read populates neither in any useful way and
   * an older gateway may omit `summary` outright.
   *
   * It is also where `isAdmin` comes from: `scope` is the only administrator
   * signal the gateway gives this client.
   */
  summary: () =>
    queryOptions({
      queryKey: [consoleScopeKey(), "usage", "summary"],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<UsageResponse & Partial<InstallationUsage>>(
          "/api/v1/admin/usage",
          { signal },
        ),
      staleTime: 60_000,
      retry: RETRY_NOT_4XX,
    }),

  /**
   * Strictly one account's inference, cross-tabbed by one dimension. Query
   * mode: rows arrive in `grouped`. Used when the caller's reads are
   * installation-wide and must be pinned to one account — their own, or on the
   * administrator activity switcher, somebody else's.
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

/**
 * The administrator reads.
 *
 * Every one of these is refused for a member — the gateway answers 403 rather
 * than narrowing, unlike the member reads above. Two things follow. The
 * administration screens are reached only from a group `adminSectionsFor`
 * hides from a member, and `AdminGate` answers anyone who arrives by another
 * route, so a member never sits in front of a page of refusals. And where one
 * of these reads is made from a screen a member *does* reach — the people
 * directory behind the sandbox fleet's owner line — it is enabled on
 * `administers` rather than fired and refused.
 *
 * All reads. The single administrator write is `sandboxMutations.adminCancel`,
 * which needs the write scope on top of the role; everything else hands off to
 * the gateway's own console, where those writes live.
 *
 * Every response type comes from the generated document (`gatewayAdmin.ts`);
 * nothing on this surface is hand-transcribed.
 */
export const adminQueries = {
  /**
   * The directory. The endpoint takes a server-side `search`, but the whole
   * directory is small enough to hold and narrow on the device — one read, and
   * then typing costs nothing.
   */
  people: () =>
    queryOptions({
      queryKey: [consoleScopeKey(), "admin", "people"],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<PersonPage>(
          `/api/v1/admin/people?limit=${PEOPLE_PAGE_SIZE}`,
          { signal },
        ),
      staleTime: 60_000,
      retry: RETRY_NOT_4XX,
    }),

  teams: () =>
    queryOptions({
      queryKey: [consoleScopeKey(), "admin", "teams"],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<TeamPage>("/api/v1/admin/teams", {
          signal,
        }),
      staleTime: 60_000,
      retry: RETRY_NOT_4XX,
    }),

  modelProviders: () =>
    queryOptions({
      queryKey: [consoleScopeKey(), "admin", "model-providers"],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<ModelProviderPage>(
          "/api/v1/admin/model-providers",
          { signal },
        ),
      staleTime: 300_000,
      retry: RETRY_NOT_4XX,
    }),

  providerModels: () =>
    queryOptions({
      queryKey: [consoleScopeKey(), "admin", "models"],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<ProviderModelPage>("/api/v1/admin/models", {
          signal,
        }),
      staleTime: 300_000,
      retry: RETRY_NOT_4XX,
    }),

  /** Every cap on the installation — not the caller-scoped `limitQueries.mine`. */
  costLimits: () =>
    queryOptions({
      queryKey: [consoleScopeKey(), "admin", "cost-limits"],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<CostLimitPage>("/api/v1/admin/cost-limits", {
          signal,
        }),
      staleTime: 60_000,
      retry: RETRY_NOT_4XX,
    }),

  costControlSettings: () =>
    queryOptions({
      queryKey: [consoleScopeKey(), "admin", "cost-control-settings"],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<CostControlSettings>(
          "/api/v1/admin/cost-controls/settings",
          { signal },
        ),
      staleTime: 300_000,
      retry: RETRY_NOT_4XX,
    }),

  guardrailPolicies: () =>
    queryOptions({
      queryKey: [consoleScopeKey(), "admin", "guardrail-policies"],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<GuardrailPolicyPage>(
          "/api/v1/admin/guardrails/policies",
          { signal },
        ),
      staleTime: 300_000,
      retry: RETRY_NOT_4XX,
    }),

  /**
   * Evaluation counts over a stated window. The window is sent rather than
   * left to the endpoint's default so the screen can name it honestly; the
   * gateway refuses an out-of-range value rather than clamping it.
   */
  guardrailActivity: (days: number) =>
    queryOptions({
      queryKey: [consoleScopeKey(), "admin", "guardrail-activity", days],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<GuardrailActivityPage>(
          `/api/v1/admin/guardrails/activity?days=${days}`,
          { signal },
        ),
      staleTime: 60_000,
      retry: RETRY_NOT_4XX,
    }),

  connectedApps: () =>
    queryOptions({
      queryKey: [consoleScopeKey(), "admin", "connected-apps"],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<ConnectedAppPage>(
          "/api/v1/admin/connected-apps",
          { signal },
        ),
      staleTime: 300_000,
      retry: RETRY_NOT_4XX,
    }),

  mcpEndpoints: () =>
    queryOptions({
      queryKey: [consoleScopeKey(), "admin", "mcp-endpoints"],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<McpEndpointPage>(
          "/api/v1/admin/mcp-endpoints",
          { signal },
        ),
      staleTime: 300_000,
      retry: RETRY_NOT_4XX,
    }),

  authenticationPolicy: () =>
    queryOptions({
      queryKey: [consoleScopeKey(), "admin", "authentication-policy"],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<AuthenticationPolicy>(
          "/api/v1/admin/authentication-policy",
          { signal },
        ),
      staleTime: 300_000,
      retry: RETRY_NOT_4XX,
    }),

  identityProviders: () =>
    queryOptions({
      queryKey: [consoleScopeKey(), "admin", "identity-providers"],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<IdentityProviderPage>(
          "/api/v1/admin/identity-providers",
          { signal },
        ),
      staleTime: 300_000,
      retry: RETRY_NOT_4XX,
    }),

  scimConnectors: () =>
    queryOptions({
      queryKey: [consoleScopeKey(), "admin", "scim-connectors"],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<ScimConnectorPage>(
          "/api/v1/admin/scim-connectors",
          { signal },
        ),
      staleTime: 300_000,
      retry: RETRY_NOT_4XX,
    }),

  /**
   * One page of the ledger, newest activity first.
   *
   * The cursor is a keyset the gateway hands back — opaque, and never built by
   * this client. Pages are separate queries keyed by the cursor that produced
   * them, so "load more" adds a page instead of refetching the ones already
   * read, and a pull refreshes every loaded page in place.
   */
  auditEvents: (cursor: string | null) =>
    queryOptions({
      queryKey: [consoleScopeKey(), "admin", "audit-events", cursor],
      queryFn: ({ signal }) =>
        controlPlaneClient().request<AuditEventPage>(
          `/api/v1/admin/audit-events?limit=${AUDIT_PAGE_SIZE}${
            cursor ? `&cursor=${encodeURIComponent(cursor)}` : ""
          }`,
          { signal },
        ),
      staleTime: 30_000,
      retry: RETRY_NOT_4XX,
    }),
};
