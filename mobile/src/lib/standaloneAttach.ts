/**
 * Standalone attach: reaching a Tidebreak machine with no gateway in the path
 * (#3404, decision 98).
 *
 * The gateway sequence in `autoAttach.ts` can trust a lot before it sends a
 * credential — the gateway named the machine, the machine echoes a resource
 * this client derived independently, and both must agree on the deployment.
 * A standalone machine offers none of that. Its discovery document in
 * `static_token` mode names the mode and the version handshake's two keys
 * (`version` and `api_level`) and nothing else: no installation id, no name,
 * no resource (`crates/tidebreak-server/src/auth.rs`). So this sequence
 * establishes what it can, in the order that keeps the credential off the
 * wire longest:
 *
 * 1. **The URL.** `validatedBaseUrl` refuses anything but `https://`, and
 *    `http://` only to a loopback host. That is the whole TLS posture for a
 *    LAN machine (decision 98) — Expo exposes no certificate hooks, so there
 *    is no place to pin or to accept a self-signed certificate.
 * 2. **The token's shape**, against the roster's own rules. A mistyped paste
 *    becomes a message here rather than a 401 from a machine that may not be
 *    reachable at all.
 * 3. **The mode.** `static_token` proceeds; `gateway`, `oidc` and `local` are
 *    each refused with their own reason, because each has a different answer.
 *    Then the API level: a machine this app does not read is refused with a
 *    message that says whether to update the app or the machine.
 * 4. **The token**, at `/auth/token-sign-in`, which is the machine's own
 *    public bootstrap probe and the first moment the credential is sent.
 *
 * The resource recorded on the attached machine is derived locally, exactly as
 * the gateway path derives it, so `MachineClient` and every screen behind it
 * keep one shape. On a standalone connection it is an internal key for the
 * credential store and nothing more: no authorization server ever sees it.
 */

import {
  AttachError,
  REASON_TOKEN_MALFORMED,
  discoverMachine,
  machineTarget,
  probeStaticToken,
} from "./attach";
import type { HttpFetch } from "./http";
import { machineTokenProblem } from "./machineToken";
import type { AttachedMachine } from "./types";

export type StandaloneAttachStage = "discover" | "verify" | "probe";

export type StandaloneAttachDeps = {
  onStage?: (stage: StandaloneAttachStage) => void;
  fetchImpl?: HttpFetch;
};

/** Why a token cannot be used, phrased for the attach screen. */
export function machineTokenMessage(raw: string): string | null {
  switch (machineTokenProblem(raw)) {
    case null:
      return null;
    case "empty":
      return "Paste the token your machine's operator gave you, or scan its code.";
    case "too_short":
      return "That is too short to be a Tidebreak machine token — they are at least 32 characters.";
    case "too_long":
      return "That is longer than any Tidebreak machine token.";
    case "charset":
      return "A machine token contains only letters, digits, and the characters . _ ~ - — check for a stray space or line break.";
  }
}

/**
 * Discovery, mode verification, then the authenticated sign-in probe.
 *
 * Persisting is the caller's job, so this stays a pure network sequence the
 * attach screen can run and a test can drive with an injected transport.
 */
export async function attachStandaloneMachine(
  machineUrl: string,
  token: string,
  deps: StandaloneAttachDeps = {},
): Promise<AttachedMachine> {
  const problem = machineTokenMessage(token);
  if (problem) {
    // Before the URL is even resolved: a bad token is the user's to fix, and
    // reporting it after a ten-second discovery timeout would bury it.
    throw new AttachError("validate", REASON_TOKEN_MALFORMED, problem);
  }
  deps.onStage?.("discover");
  const discovered = await discoverMachine(
    machineUrl,
    machineTarget(),
    deps.fetchImpl,
  );
  deps.onStage?.("verify");
  deps.onStage?.("probe");
  await probeStaticToken(discovered.baseUrl, token.trim(), deps.fetchImpl);
  return { baseUrl: discovered.baseUrl, resource: discovered.resource };
}
