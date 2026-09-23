import {
  assertResourceEcho,
  tidebreakMachineResource,
} from "./resource";
import { API_LEVEL, MIN_API_LEVEL } from "../generated/wire";
import { fetchRefusingRedirects, type HttpFetch, type HttpResponse } from "./http";
import {
  REASON_REQUIRES_TLS,
  REASON_URL_INVALID,
  UrlValidationError,
  urlsMatch,
  validatedBaseUrl,
} from "./url";
import type { AuthDiscovery } from "./types";

export const DISCOVERY_TIMEOUT_MS = 10_000;

export const REASON_UNREACHABLE = "unreachable";
export const REASON_NOT_A_MACHINE = "not_a_machine";
export const REASON_GATEWAY_MISMATCH = "gateway_mismatch";
export const REASON_RESOURCE_MISMATCH = "resource_mismatch";
export const REASON_TOKEN_REFUSED = "token_refused";
export const REASON_GATEWAY_AUTH_UNAVAILABLE = "gateway_auth_unavailable";

/**
 * Standalone attach was asked for, but this machine authenticates through a
 * Model Gateway. Distinct from the refusals below because it is the one case
 * with a working answer: pair the gateway instead.
 */
export const REASON_GATEWAY_MACHINE = "gateway_machine";

/**
 * The machine signs people in through an OpenID Connect provider. Its
 * authenticator resolves only the `tb_oidc_` bearers it mints for a browser
 * (`PrincipalAuthenticator::Oidc` in `crates/tidebreak-server/src/auth.rs`),
 * so a roster token presented here names nobody even when the operator keeps a
 * token file beside OIDC for CLI access.
 */
export const REASON_OIDC_UNSUPPORTED = "oidc_unsupported";

/**
 * The machine is a desktop app's loopback server: its per-launch bearer names
 * nobody on a shared deployment and is not something an operator can hand out.
 */
export const REASON_LOCAL_ONLY = "local_only";

/** The token is shaped in a way no roster token can be. */
export const REASON_TOKEN_MALFORMED = "token_malformed";

/** The machine serves a newer API level than this app reads. Update the app. */
export const REASON_APP_TOO_OLD = "app_too_old";

/**
 * The machine serves an older API level than this app still reads. Update the
 * machine.
 */
export const REASON_MACHINE_TOO_OLD = "machine_too_old";

/**
 * The token names a `service` principal. Ordinary member routes accept one —
 * a service owns and runs sessions — but `/auth/token-sign-in` is deliberately
 * narrower, and a phone is a sign-in-shaped client, not an automation.
 */
export const REASON_TOKEN_IS_SERVICE = "token_is_service";

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
  /**
   * The gateway that vouches for this machine, present only for a gateway
   * attach. A standalone machine names none — there is no gateway in its path
   * at all — which is exactly what its discovery document says.
   */
  gatewayUrl?: string;
};

/**
 * How a machine is expected to authenticate, chosen by the kind of connection
 * attaching it.
 *
 * `gateway` requires the machine to speak Model Gateway auth, echo the
 * resource this client derived, and name the paired deployment.
 *
 * `machine` (#3404) is standalone attach: a direct URL whose discovery answers
 * `static_token`, authenticated by a long-lived roster token the operator
 * hands over. Its discovery document carries no gateway and no resource to
 * check an echo against, so verification there is what the *app* can establish
 * on its own — the URL survived `validatedBaseUrl` (TLS or loopback, no
 * credentials, no redirect), the mode is the one mode a phone can hold a
 * credential for, and the token authenticates against the machine itself.
 *
 * Keeping the two as a union rather than a boolean is what makes the wrong
 * pairing legible instead of silent: a gateway-authenticated machine reached
 * through standalone attach is refused with an answer ("pair the gateway"),
 * not with a generic failure.
 */
export type AttachTarget = { kind: "gateway"; gatewayUrl: string } | { kind: "machine" };

export function gatewayTarget(gatewayUrl: string): AttachTarget {
  return { kind: "gateway", gatewayUrl };
}

export function machineTarget(): AttachTarget {
  return { kind: "machine" };
}

/**
 * Discovery, then the per-kind verification. The echoed resource is never
 * trusted: the caller's derivation from the URL the user entered is, and the
 * echo only has to agree with it.
 */
export async function discoverMachine(
  machineUrl: string,
  target: AttachTarget,
  fetchImpl?: HttpFetch,
): Promise<DiscoveredMachine> {
  let baseUrl: string;
  try {
    baseUrl = validatedBaseUrl(machineUrl);
  } catch (error) {
    // The URL rule's own reason is carried, not flattened: `requires_tls` is
    // the one refusal with a different answer for the user than "that is not a
    // usable URL", and the epic's done-when asks for it by name.
    throw new AttachError(
      "validate",
      error instanceof UrlValidationError ? error.reason : REASON_URL_INVALID,
      error instanceof UrlValidationError && error.reason === REASON_REQUIRES_TLS
        ? "A machine outside this phone must be reached over HTTPS."
        : "The machine URL is not usable.",
    );
  }
  const derived = tidebreakMachineResource(baseUrl);
  let response: HttpResponse;
  // A machine on a private network (VPN-only hosted deployments) often drops
  // packets instead of refusing the connection, so the fetch would hang
  // forever. Race it against a deadline; the abort signal is best-effort for
  // transports that honor it.
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), DISCOVERY_TIMEOUT_MS);
  try {
    response = await Promise.race([
      fetchRefusingRedirects(
        `${baseUrl}/auth/discovery`,
        { signal: controller.signal },
        fetchImpl,
      ),
      new Promise<never>((_, reject) => {
        controller.signal.addEventListener("abort", () => {
          reject(
            new Error(
              `No response after ${DISCOVERY_TIMEOUT_MS / 1000} seconds.`,
            ),
          );
        });
      }),
    ]);
  } catch (error) {
    throw new AttachError(
      "discover",
      REASON_UNREACHABLE,
      error instanceof Error ? error.message : "The machine did not respond.",
    );
  } finally {
    clearTimeout(timer);
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
  if (target.kind === "machine") {
    verifyStandalone(discovery);
    requireCompatibleMachine(discovery);
    return { baseUrl, resource: derived };
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
    if (!urlsMatch(discovery.gateway_url, target.gatewayUrl)) {
      throw new Error("mismatch");
    }
  } catch {
    throw new AttachError(
      "verify",
      REASON_GATEWAY_MISMATCH,
      "The machine named a different gateway than the one you paired.",
    );
  }
  requireCompatibleMachine(discovery);
  return {
    baseUrl,
    resource: derived,
    gatewayUrl: validatedBaseUrl(target.gatewayUrl),
  };
}

/** The API levels this app reads, from its copy of the generated wire types. */
export const SUPPORTED_API_LEVELS = { min: MIN_API_LEVEL, max: API_LEVEL };

/**
 * Refuse a machine whose API level this app does not read.
 *
 * The discovery document carries `version` and `api_level` beside its mode.
 * A machine from before the version handshake leaves both out, and attaches
 * as it always has. Runs after the mode checks, so a machine this phone could
 * never attach is refused for that reason first.
 */
export function requireCompatibleMachine(
  discovery: Pick<AuthDiscovery, "version" | "api_level">,
  supported: { min: number; max: number } = SUPPORTED_API_LEVELS,
): void {
  const level = discovery.api_level;
  if (typeof level !== "number" || !Number.isSafeInteger(level)) return;
  const version = displayableVersion(discovery.version);
  if (level > supported.max) {
    throw new AttachError(
      "discover",
      REASON_APP_TOO_OLD,
      version
        ? `This machine runs Tidebreak ${version}, which is newer than this app supports. Update the app to connect.`
        : "This machine runs a newer Tidebreak than this app supports. Update the app to connect.",
    );
  }
  if (level < supported.min) {
    throw new AttachError(
      "discover",
      REASON_MACHINE_TOO_OLD,
      version
        ? `This machine runs Tidebreak ${version}, which this app no longer supports. Update the machine to connect.`
        : "This machine runs an older Tidebreak that this app no longer supports. Update the machine to connect.",
    );
  }
}

/** A release string short and plain enough to put in a sentence. */
function displayableVersion(value: unknown): string | null {
  return typeof value === "string" && /^[0-9A-Za-z.+-]{1,32}$/.test(value)
    ? value
    : null;
}

/**
 * The standalone half of verification: which discovery modes a phone holding a
 * roster token can and cannot attach to.
 *
 * Every refusal is its own reason, because each has a different answer for the
 * person holding the phone, and a single "unsupported mode" message would
 * leave all three looking like the same bug.
 */
function verifyStandalone(discovery: AuthDiscovery): void {
  switch (discovery.mode) {
    case "static_token":
      return;
    case "gateway":
      throw new AttachError(
        "verify",
        REASON_GATEWAY_MACHINE,
        "This machine authenticates through Model Gateway. Pair that gateway instead — it attaches this machine for you.",
      );
    case "oidc":
      throw new AttachError(
        "verify",
        REASON_OIDC_UNSUPPORTED,
        "This machine signs people in through an identity provider, and only issues browser sessions. A token from its roster will not authenticate here.",
      );
    case "local":
      throw new AttachError(
        "verify",
        REASON_LOCAL_ONLY,
        "This is a desktop Tidebreak on its own machine. Its per-launch token names nobody, so it cannot be attached from a phone.",
      );
    default:
      throw new AttachError(
        "verify",
        REASON_NOT_A_MACHINE,
        "This machine reported an authentication mode this app does not know.",
      );
  }
}

/**
 * Whether a static token authenticates against this machine, and whether it
 * names somebody who signs in.
 *
 * `POST /auth/token-sign-in` is the machine's own public bootstrap probe
 * (decision 87): it answers `204` for a roster token that names a person,
 * `401` for one it cannot resolve, and `403` for a `service` principal, which
 * owns automated sessions and deliberately does not sign in. The token travels
 * in the header and appears in no URL, no log line, and no error message here.
 */
export async function probeStaticToken(
  machineUrl: string,
  token: string,
  fetchImpl?: HttpFetch,
): Promise<void> {
  let response: HttpResponse;
  try {
    response = await fetchRefusingRedirects(
      `${machineUrl}/auth/token-sign-in`,
      { method: "POST", headers: { Authorization: `Bearer ${token}` } },
      fetchImpl,
    );
  } catch (error) {
    throw new AttachError(
      "probe",
      REASON_UNREACHABLE,
      error instanceof Error
        ? error.message
        : "The machine did not respond to the sign-in probe.",
    );
  }
  if (response.status === 401) {
    throw new AttachError(
      "probe",
      REASON_TOKEN_REFUSED,
      "The machine did not recognize that token. Check it against the deployment's token file, or ask for a fresh one.",
    );
  }
  if (response.status === 403) {
    throw new AttachError(
      "probe",
      REASON_TOKEN_IS_SERVICE,
      "That token belongs to a service account, which cannot sign in. Use a token that names a person.",
    );
  }
  if (response.status === 404) {
    // Discovery said `static_token` a moment ago; a 404 here means the route
    // is absent, so this build predates standalone sign-in.
    throw new AttachError(
      "probe",
      REASON_NOT_A_MACHINE,
      "This machine does not offer token sign-in. It may be running a build older than this app supports.",
    );
  }
  if (!response.ok) {
    throw new AttachError(
      "probe",
      REASON_NOT_A_MACHINE,
      `The machine could not check that token (HTTP ${response.status}).`,
    );
  }
}

export async function probePolicy(
  machineUrl: string,
  accessToken: string,
  fetchImpl?: HttpFetch,
): Promise<void> {
  let response: HttpResponse;
  try {
    response = await fetchRefusingRedirects(
      `${machineUrl}/policy`,
      { headers: { Authorization: `Bearer ${accessToken}` } },
      fetchImpl,
    );
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
