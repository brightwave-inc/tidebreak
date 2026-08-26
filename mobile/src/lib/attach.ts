import {
  assertResourceEcho,
  tidebreakMachineResource,
} from "./resource";
import { fetchRefusingRedirects } from "./http";
import { urlsMatch, validatedBaseUrl } from "./url";
import type { AuthDiscovery } from "./types";

export const REASON_UNREACHABLE = "unreachable";
export const REASON_NOT_A_MACHINE = "not_a_machine";
export const REASON_GATEWAY_MISMATCH = "gateway_mismatch";
export const REASON_RESOURCE_MISMATCH = "resource_mismatch";
export const REASON_TOKEN_REFUSED = "token_refused";
export const REASON_GATEWAY_AUTH_UNAVAILABLE = "gateway_auth_unavailable";

export class AttachError extends Error {
  readonly reason: string;
  readonly stage: "validate" | "discover" | "verify" | "probe";

  constructor(
    stage: AttachError["stage"],
    reason: string,
    message: string,
  ) {
    super(message);
    this.name = "AttachError";
    this.stage = stage;
    this.reason = reason;
  }
}

export type DiscoveredMachine = {
  baseUrl: string;
  resource: string;
  gatewayUrl: string;
};

export async function discoverMachine(
  machineUrl: string,
  pairedGatewayUrl: string,
  fetchImpl: typeof fetch = fetch,
): Promise<DiscoveredMachine> {
  let baseUrl: string;
  try {
    baseUrl = validatedBaseUrl(machineUrl);
  } catch (error) {
    throw new AttachError(
      "validate",
      "url_invalid",
      error instanceof Error ? error.message : "The machine URL is not usable.",
    );
  }
  const derived = tidebreakMachineResource(baseUrl);
  let response: Response;
  try {
    response = await fetchRefusingRedirects(
      fetchImpl,
      `${baseUrl}/auth/discovery`,
    );
  } catch (error) {
    throw new AttachError(
      "discover",
      REASON_UNREACHABLE,
      error instanceof Error ? error.message : "The machine did not respond.",
    );
  }
  if (!response.ok) {
    throw new AttachError(
      "discover",
      REASON_NOT_A_MACHINE,
      `Discovery failed (HTTP ${response.status}).`,
    );
  }
  let discovery: AuthDiscovery;
  try {
    discovery = (await response.json()) as AuthDiscovery;
  } catch {
    throw new AttachError(
      "discover",
      REASON_NOT_A_MACHINE,
      "Discovery did not return JSON.",
    );
  }
  if (discovery.mode !== "gateway") {
    throw new AttachError(
      "discover",
      REASON_GATEWAY_AUTH_UNAVAILABLE,
      "This machine is not using Model Gateway authentication.",
    );
  }
  if (!discovery.gateway_url || !discovery.resource) {
    throw new AttachError(
      "discover",
      REASON_GATEWAY_AUTH_UNAVAILABLE,
      "Discovery did not name a gateway and resource.",
    );
  }
  try {
    assertResourceEcho(derived, discovery.resource);
  } catch {
    throw new AttachError(
      "verify",
      REASON_RESOURCE_MISMATCH,
      "The machine echoed a resource that does not match this URL. Refusing to attach.",
    );
  }
  try {
    if (!urlsMatch(discovery.gateway_url, pairedGatewayUrl)) {
      throw new Error("mismatch");
    }
  } catch {
    throw new AttachError(
      "verify",
      REASON_GATEWAY_MISMATCH,
      "The machine named a different gateway than the one you paired.",
    );
  }
  return {
    baseUrl,
    resource: derived,
    gatewayUrl: validatedBaseUrl(pairedGatewayUrl),
  };
}

export async function probePolicy(
  machineUrl: string,
  accessToken: string,
  fetchImpl: typeof fetch = fetch,
): Promise<void> {
  let response: Response;
  try {
    response = await fetchRefusingRedirects(fetchImpl, `${machineUrl}/policy`, {
      headers: { Authorization: `Bearer ${accessToken}` },
    });
  } catch (error) {
    throw new AttachError(
      "probe",
      REASON_UNREACHABLE,
      error instanceof Error ? error.message : "The machine did not respond to /policy.",
    );
  }
  if (response.status === 401 || response.status === 403) {
    throw new AttachError(
      "probe",
      REASON_TOKEN_REFUSED,
      "The machine refused the minted access token.",
    );
  }
  if (!response.ok) {
    throw new AttachError(
      "probe",
      REASON_NOT_A_MACHINE,
      `Authenticated probe failed (HTTP ${response.status}).`,
    );
  }
}

export function workspaceDisplayName(item: {
  title?: string | null;
  name?: string | null;
  id: string;
}): string {
  return item.title?.trim() || item.name?.trim() || item.id;
}

export function parseWorkspaceList(json: unknown): { id: string; title?: string | null; name?: string | null }[] {
  if (Array.isArray(json)) {
    return json.filter(isWorkspaceLike);
  }
  if (json && typeof json === "object") {
    const record = json as Record<string, unknown>;
    for (const key of ["workspaces", "items", "sessions"]) {
      const value = record[key];
      if (Array.isArray(value)) {
        return value.filter(isWorkspaceLike);
      }
    }
  }
  return [];
}

function isWorkspaceLike(
  value: unknown,
): value is { id: string; title?: string | null; name?: string | null } {
  return (
    !!value &&
    typeof value === "object" &&
    typeof (value as { id?: unknown }).id === "string"
  );
}
