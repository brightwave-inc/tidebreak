/**
 * Named handles on the generated gateway admin (control-plane) types.
 *
 * `src/generated/gatewayAdmin.ts` is 20k lines of `paths` / `operations` /
 * `components` indexed by URL and operationId. Screens should not spell those
 * index chains out; they import the aliases from here, so a renamed schema
 * breaks in one file rather than in every consumer.
 *
 * The admin surface only. Member (`/api/v1/cli/*`) and runtime types stay
 * hand-maintained in `types.ts` until the gateway annotates those routers.
 * The console screens that consume this arrive in #3402.
 */
import type { components, operations } from "@/generated/gatewayAdmin";

/**
 * The JSON body one admin operation returns on success, by operationId.
 *
 * `never` for an operation with no 200 JSON body (a 201 or 204), which makes
 * misuse a type error at the call site rather than an `unknown` downstream.
 */
export type GatewayAdminResult<Op extends keyof operations> =
  operations[Op] extends {
    responses: { 200: { content: { "application/json": infer Body } } };
  }
    ? Body
    : never;

/** The query parameters one admin operation accepts, by operationId. */
export type GatewayAdminQuery<Op extends keyof operations> =
  operations[Op] extends { parameters: { query?: infer Query } }
    ? NonNullable<Query>
    : Record<string, never>;

/** One page of the audit ledger, newest activity first. */
export type AuditEventPage = GatewayAdminResult<"listAuditEvents">;

/** One recorded administrative action. */
export type AuditEvent = components["schemas"]["AuditEventView"];

/** The filters the audit ledger read accepts. */
export type AuditEventQuery = GatewayAdminQuery<"listAuditEvents">;

/** Gateway-hosted sandboxes matching a read. Unpaged: the gateway caps it. */
export type SandboxPage = GatewayAdminResult<"listSandboxes">;

/** One gateway-hosted sandbox. */
export type Sandbox = components["schemas"]["SandboxView"];

/**
 * The cursor for the next page, or null at the end of the ledger.
 *
 * The gateway omits `next_cursor` on the last page and may also send it as
 * explicit null; both mean "stop", and callers should not have to know that.
 */
export function nextAuditCursor(page: AuditEventPage): string | null {
  return page.next_cursor ?? null;
}
