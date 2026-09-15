/**
 * Gateway clients, one per resource, per connection.
 *
 * The gateway mints one access token per resource, so the console is three
 * clients over one base URL and one refresh family:
 *
 * - `control` — the CLI self-service surface every paired session already
 *   holds: identity, the member catalog, subscriptions, shared apps.
 * - `control_plane` — the versioned admin and observability reads. A session
 *   whose consent did not carry them is refused at the mint with
 *   `invalid_resource`, which `TokenStore` raises as `ResourceRefusedError`
 *   rather than a sign-out.
 * - `runtime:<slug>` — the owner-scoped sandbox verbs. See `runtimeSlug.ts`
 *   for why the slug is discovered but never required.
 *
 * Every factory resolves the active connection at call time rather than
 * capturing it. Switching connections switches which refresh family mints the
 * next token, and a client captured at mount would keep talking to the
 * previous gateway.
 */

import { GatewayClient } from "../lib/gatewayClient";
import { isGatewayConnection } from "../lib/connections";
import type { AppListResponse } from "../lib/consoleTypes";
import {
  RESOURCE_CONTROL,
  RESOURCE_CONTROL_PLANE,
  runtimeResource,
} from "../lib/resource";
import { runtimeSlugFrom } from "../lib/runtimeSlug";
import { connections } from "./runtime";

/** The active gateway's base URL, or a throw a screen can render. */
function activeGatewayUrl(): string {
  const active = connections.active();
  if (!isGatewayConnection(active)) {
    throw new Error("No gateway connection is active.");
  }
  return active.gatewayUrl;
}

function clientFor(resource: string): GatewayClient {
  return new GatewayClient({
    baseUrl: activeGatewayUrl(),
    resource,
    // Resolved per call, not captured, for the reason in the module note.
    tokens: {
      getAccessToken: (wanted) =>
        connections.activeTokens().getAccessToken(wanted),
    },
  });
}

/** The CLI self-service surface. Available to every paired session. */
export function controlClient(): GatewayClient {
  return clientFor(RESOURCE_CONTROL);
}

/** The versioned console reads. Only a console-granted session can mint this. */
export function controlPlaneClient(): GatewayClient {
  return clientFor(RESOURCE_CONTROL_PLANE);
}

/**
 * One resolved runtime slug per connection.
 *
 * Cached because discovery costs a `control` round trip and the answer cannot
 * change while a connection lives: it is a property of the installation's app
 * listing, not of the run being steered. Keyed by connection id so two paired
 * gateways never share one.
 */
const runtimeSlugs = new Map<string, string>();

export async function runtimeClient(): Promise<GatewayClient> {
  const active = connections.active();
  if (!active) {
    throw new Error("No gateway connection is active.");
  }
  let slug = runtimeSlugs.get(active.id);
  if (!slug) {
    let apps: AppListResponse | null = null;
    try {
      apps = await controlClient().request<AppListResponse>(
        "/api/v1/cli/apps",
      );
    } catch {
      // No catalog reach is not fatal: the verbs bind to the user, so the
      // fallback slug mints a token that works just as well.
      apps = null;
    }
    slug = runtimeSlugFrom(apps);
    runtimeSlugs.set(active.id, slug);
  }
  return clientFor(runtimeResource(slug));
}
