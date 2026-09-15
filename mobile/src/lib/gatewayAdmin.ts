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
 * The installation-wide halves of the usage read.
 *
 * The member surfaces read the same endpoint through the hand-maintained
 * `UsageResponse` in `consoleTypes.ts`; these are the two rollups only an
 * administrator's unfiltered read populates, so they come from the generated
 * document rather than being transcribed a second time.
 */
export type TeamUsageRow = components["schemas"]["TeamUsageView"];
export type UsageSummary = components["schemas"]["UsageSummaryView"];
export type InstallationUsage = Pick<
  GatewayAdminResult<"listUsage">,
  "by_team" | "summary"
>;

/** The user directory. */
export type PersonPage = GatewayAdminResult<"listPeople">;
export type Person = components["schemas"]["PersonView"];

/** Teams and the reach each one carries. */
export type TeamPage = GatewayAdminResult<"listTeams">;
export type Team = components["schemas"]["TeamView"];

/** Inference providers and the catalog rows they own. */
export type ModelProviderPage = GatewayAdminResult<"listModelProviders">;
export type ModelProvider = components["schemas"]["ProviderView"];
export type ProviderModelPage = GatewayAdminResult<"listProviderModels">;
export type ProviderModel = components["schemas"]["ModelView"];
export type ProvisionedCapacityMode =
  components["schemas"]["ProvisionedCapacityMode"];

/** Every cap configured on the installation, not only the caller's own. */
export type CostLimitPage = GatewayAdminResult<"listCostLimits">;
export type CostLimit = components["schemas"]["CostLimitView"];
export type CostControlSettings = GatewayAdminResult<"getCostControlSettings">;

/** Guardrails: what the engines saw, and what is configured to see it. */
export type GuardrailActivityPage = GatewayAdminResult<"getGuardrailActivity">;
export type GuardrailActivityRow =
  components["schemas"]["GuardrailActivityView"];
export type GuardrailPolicyPage = GatewayAdminResult<"listGuardrailPolicies">;
export type GuardrailPolicy = components["schemas"]["GuardrailPolicyView"];

/** How this gateway is wired: apps, endpoints, and identity. */
export type ConnectedAppPage = GatewayAdminResult<"listConnectedApps">;
export type ConnectedApp = components["schemas"]["ConnectedAppView"];
export type McpEndpointPage = GatewayAdminResult<"listMcpEndpoints">;
export type McpEndpoint = components["schemas"]["McpEndpointView"];
export type AuthenticationPolicy =
  GatewayAdminResult<"getAuthenticationPolicy">;
export type IdentityProviderPage =
  GatewayAdminResult<"listIdentityProviders">;
export type IdentityProvider = components["schemas"]["IdentityProviderView"];
export type ScimConnectorPage = GatewayAdminResult<"listScimConnectors">;
export type ScimConnector = components["schemas"]["ScimConnectorView"];

/**
 * The cursor for the next page, or null at the end of the ledger.
 *
 * The gateway omits `next_cursor` on the last page and may also send it as
 * explicit null; both mean "stop", and callers should not have to know that.
 */
export function nextAuditCursor(page: AuditEventPage): string | null {
  return page.next_cursor ?? null;
}
