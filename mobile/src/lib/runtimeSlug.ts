/**
 * Which `runtime:<slug>` resource the owner-scoped sandbox verbs mint.
 *
 * The slug nominally names an MCP endpoint, and it reads as though the caller
 * has to know one. It does not, for two reasons that hold on the gateway side:
 *
 * - **The mint is syntactic.** `resource_scope` in mg `cli_auth.rs` accepts
 *   any `runtime:<identifier>` of 1..=127 characters drawn from
 *   `[A-Za-z0-9_-]`, and grants it `runtime:execute`. It never checks the
 *   identifier against an endpoint that exists.
 * - **The verbs bind to the user, not to the endpoint.** Read, cancel, and
 *   the steering inbox authenticate through `authenticate_sandbox_access`
 *   (mg `runtime_protocol.rs`), which resolves the caller's own runtime token
 *   and then matches the sandbox by `user_id`. A sandbox outlives the endpoint
 *   session that spawned it and may be steered from a different client
 *   entirely, so no route compares the token's slug to anything.
 *
 * So a literal fallback always works, which is why one is kept rather than
 * failing when discovery finds nothing: a member on an installation with no
 * MCP endpoints at all can still steer and cancel their own runs, which is the
 * whole point of the surface.
 *
 * Discovery runs first anyway. Naming a slug the installation really serves
 * makes the minted token's audience meaningful in the gateway's own audit
 * trail, which a client-invented name is not. Tidewatch used its own client
 * name as the fallback; this app uses `tidebreak-mobile`, its OAuth client id —
 * deliberately not the bare `tidebreak`, which is the hosted add-on's own
 * runtime audience (`runtime:tidebreak`, mg `cli_auth.rs`) and would put the
 * phone's steering token in the same namespace as the add-on's delegation.
 */

import type { AppListResponse } from "./consoleTypes";

/**
 * The slug used when the catalog names none. A well-formed identifier under
 * the gateway's rule, and unmistakably this client.
 */
export const FALLBACK_RUNTIME_SLUG = "tidebreak-mobile";

/**
 * The first MCP endpoint slug in the caller's entitled-app listing, or the
 * fallback. Any entitled endpoint's slug serves equally — see the module note
 * — so "first" is a choice of one, not a ranking.
 */
export function runtimeSlugFrom(
  apps: AppListResponse | null | undefined,
): string {
  const found = (apps?.apps ?? [])
    .flatMap((app) => app.mcp_endpoint_slugs ?? [])
    .find((slug) => isMintableSlug(slug));
  return found ?? FALLBACK_RUNTIME_SLUG;
}

/**
 * Whether the gateway's token endpoint would accept `runtime:<slug>`. Applied
 * to discovered values because a slug this client cannot mint is worse than no
 * discovery at all: it would turn a working fallback into a refused mint.
 */
export function isMintableSlug(slug: string): boolean {
  return (
    slug.length > 0 && slug.length <= 127 && /^[A-Za-z0-9_-]+$/.test(slug)
  );
}
