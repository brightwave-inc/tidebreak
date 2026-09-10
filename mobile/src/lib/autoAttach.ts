/**
 * Attaching the machine the paired gateway advertises.
 *
 * `GET /api/v1/meta` names `tidebreak_machine_url` when the deployment hosts a
 * machine. There is no legitimate cross-gateway attach: `discoverMachine`
 * derives `tidebreak:<sha256(canonical_url)>` locally and refuses a machine
 * whose discovery echo or `gateway_url` is not the paired deployment. So on the
 * advertised-URL path a confirm screen adds no decision the user can make, and
 * pairing runs the sequence itself.
 *
 * Everything the Attach screen would have done still runs — same discovery,
 * same verification, same authenticated probe. Only the tap is gone. When any
 * of it fails, or when the gateway advertised nothing, the Attach screen is
 * still the answer, and it renders the failure this module describes.
 */

import {
  AttachError,
  REASON_UNREACHABLE,
  discoverMachine,
  probePolicy,
} from "./attach";
import type { HttpFetch } from "./http";
import type { AttachedMachine, PersistedSession } from "./types";

/** What the Attach screen renders when a machine could not be attached. */
export type AttachFailure =
  | { kind: "unreachable"; detail: string }
  | { kind: "error"; message: string };

export type AttachStage = "discover" | "verify" | "probe";

export type AttachDeps = {
  getAccessToken: (resource: string) => Promise<string>;
  onStage?: (stage: AttachStage) => void;
  fetchImpl?: HttpFetch;
};

/**
 * Discovery, resource/gateway verification, then the authenticated `/policy`
 * probe. Persisting the result is the caller's job so this stays a pure
 * network sequence both the Attach screen and pairing can run.
 */
export async function attachMachine(
  machineUrl: string,
  gatewayUrl: string,
  deps: AttachDeps,
): Promise<AttachedMachine> {
  deps.onStage?.("discover");
  const discovered = await discoverMachine(
    machineUrl,
    gatewayUrl,
    deps.fetchImpl,
  );
  deps.onStage?.("verify");
  deps.onStage?.("probe");
  const token = await deps.getAccessToken(discovered.resource);
  await probePolicy(discovered.baseUrl, token, deps.fetchImpl);
  return { baseUrl: discovered.baseUrl, resource: discovered.resource };
}

/**
 * The machine URL the gateway advertised at pairing, or null when the
 * deployment hosts no machine and the user must name one.
 */
export function advertisedMachineUrl(
  session: Pick<PersistedSession, "machinePrefillUrl"> | null,
): string | null {
  const advertised = session?.machinePrefillUrl?.trim();
  return advertised ? advertised : null;
}

export type AutoAttachOutcome =
  /** Attached; the caller persists the machine and lands on the hub. */
  | { kind: "attached"; machine: AttachedMachine }
  /**
   * The Attach screen owns it from here. `failure` is null when the gateway
   * advertised no machine at all — that is not an error, just a URL only the
   * user knows.
   */
  | { kind: "manual"; failure: AttachFailure | null };

/**
 * The auto-vs-fallback decision. Never throws: an attach that fails is a
 * routing outcome, not an exception, so the caller cannot strand the user on
 * a spinner.
 */
export async function autoAttach(
  session: Pick<PersistedSession, "gatewayUrl" | "machinePrefillUrl">,
  deps: AttachDeps,
): Promise<AutoAttachOutcome> {
  const advertised = advertisedMachineUrl(session);
  if (!advertised) {
    return { kind: "manual", failure: null };
  }
  try {
    const machine = await attachMachine(advertised, session.gatewayUrl, deps);
    return { kind: "attached", machine };
  } catch (error) {
    return { kind: "manual", failure: describeAttachFailure(error) };
  }
}

export function describeAttachFailure(error: unknown): AttachFailure {
  if (error instanceof AttachError && error.reason === REASON_UNREACHABLE) {
    return { kind: "unreachable", detail: error.message };
  }
  if (error instanceof AttachError) {
    return { kind: "error", message: `${error.stage}: ${error.message}` };
  }
  return {
    kind: "error",
    message: error instanceof Error ? error.message : "Attach failed.",
  };
}

/**
 * A failed auto-attach hands the Attach screen its error state through route
 * params; the URL itself is already the screen's prefill from the session.
 */
export function attachFailureParams(
  failure: AttachFailure,
): Record<string, string> {
  return failure.kind === "unreachable"
    ? { failure: "unreachable", detail: failure.detail }
    : { failure: "error", detail: failure.message };
}

export function attachFailureFromParams(params: {
  failure?: string | string[];
  detail?: string | string[];
}): AttachFailure | null {
  const kind = firstParam(params.failure);
  const detail = firstParam(params.detail) ?? "Attach failed.";
  if (kind === "unreachable") {
    return { kind: "unreachable", detail };
  }
  if (kind === "error") {
    return { kind: "error", message: detail };
  }
  return null;
}

function firstParam(value: string | string[] | undefined): string | undefined {
  return Array.isArray(value) ? value[0] : value;
}
