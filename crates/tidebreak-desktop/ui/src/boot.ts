import { invoke, isTauri } from "@tauri-apps/api/core";
import type { RemoteMachineState, ServerInfo } from "./api";
import {
  type HandoffFailure,
  handoffBearer,
  handoffFailure,
  hostedSession,
  markHostedSession,
} from "./hostedSession";

/**
 * Resolve how the UI reaches the API it is attached to.
 *
 * - Inside Tauri: `server_info` from the host. That is the embedded server on
 *   loopback, or — when the user has attached this client to a remote machine —
 *   that machine's URL and token, which the shell holds.
 * - In a Vite browser tab: the running desktop's `{data_dir}/listen.json`,
 *   served by the dev middleware, so `http://localhost:1420` can attach to
 *   the same `scripts/dev.sh` process.
 * - Explicit `VITE_TIDEBREAK_URL` + `VITE_TIDEBREAK_TOKEN` still win, for
 *   pointing the same React app at `tidebreak serve`.
 * - A production bundle served by a machine: the page's own origin is the
 *   API, and the bearer is the one the page was handed. See
 *   {@link hostedServerInfo}.
 *
 * The dev paths report `local`. Host authority is a property of the native
 * shell, and a browser tab has none of it either way, so nothing is gained by
 * calling those attachments remote. The hosted page reports `remote`, because
 * that is what it is: the work lives on the machine, and every control that
 * would reach this computer must know to stand down.
 */
export async function resolveServerInfo(): Promise<ServerInfo> {
  if (isTauri()) {
    return await invoke<ServerInfo>("server_info");
  }

  const fromEnv = envServerInfo();
  if (fromEnv) return fromEnv;

  const fromDesktop = await desktopListenInfo();
  if (fromDesktop) return fromDesktop;

  const fromMachine = await hostedServerInfo();
  if (fromMachine) return fromMachine;

  throw new Error(
    "No Tidebreak server is reachable from this browser tab. Keep " +
      "`scripts/dev.sh` running (it publishes a listen endpoint), or set " +
      "VITE_TIDEBREAK_URL and VITE_TIDEBREAK_TOKEN for `tidebreak serve`.",
  );
}

/** Which machine this client is attached to, without opening a connection. */
export async function remoteMachineState(): Promise<RemoteMachineState> {
  if (!isTauri()) {
    const hosted = hostedSession();
    return hosted
      ? { attachment: "remote", baseUrl: hosted.baseUrl }
      : { attachment: "local", baseUrl: null };
  }
  return await invoke<RemoteMachineState>("remote_machine_state");
}

/**
 * Thrown by boot when the page is served by a machine but holds no bearer.
 *
 * Not a boot failure: the machine answered, and the page knows exactly what
 * is missing. The shell shows the sign-in screen instead of the failure one.
 */
export class HostedSignInRequired extends Error {
  constructor(
    readonly gatewayUrl: string | null,
    /** Set when the page came from the landing route and it could not
     * hand over a bearer; the sign-in screen words the reason. */
    readonly failure: HandoffFailure | null = null,
  ) {
    super("This machine needs a session from your Model Gateway console.");
    this.name = "HostedSignInRequired";
  }
}

/** What the machine's public discovery document says about signing in. */
type AuthDiscovery =
  | { mode: "gateway"; gateway_url: string; resource: string }
  | { mode: "static_token" }
  | { mode: "local" };

const DISCOVERY_PATH = "/auth/discovery";

/**
 * The hosted branch: a production bundle whose origin is a Tidebreak machine.
 *
 * The origin is asked, not assumed. `/auth/discovery` is the one public,
 * JSON-answering route every machine has, so a page that gets a discovery
 * document back is served by a machine, and one that does not — a static
 * preview of the bundle, say — is not, and boot goes on to say nothing is
 * reachable. A page served by a machine but holding no bearer throws
 * {@link HostedSignInRequired} with the console that can mint one.
 */
export async function hostedServerInfo({
  origin = window.location.origin,
  dev = import.meta.env.DEV,
  fetch = globalThis.fetch,
  bearer = handoffBearer(),
  failure = handoffFailure(),
}: {
  origin?: string;
  dev?: boolean;
  fetch?: typeof globalThis.fetch;
  bearer?: string | null;
  failure?: HandoffFailure | null;
} = {}): Promise<ServerInfo | null> {
  // The dev server answers every path with the page, so discovery there
  // would parse the bundle's own HTML. Dev tabs have `listen.json`.
  if (dev) return null;
  const discovery = await readDiscovery(fetch, `${origin}${DISCOVERY_PATH}`);
  if (!discovery) return null;
  const gatewayUrl =
    discovery.mode === "gateway"
      ? discovery.gateway_url.replace(/\/$/, "")
      : null;
  markHostedSession({ baseUrl: origin, gatewayUrl });
  if (!bearer) throw new HostedSignInRequired(gatewayUrl, failure);
  return {
    baseUrl: origin,
    token: bearer,
    attachment: "remote",
    gatewayAuth: discovery.mode === "gateway",
  };
}

async function readDiscovery(
  fetch: typeof globalThis.fetch,
  url: string,
): Promise<AuthDiscovery | null> {
  try {
    const response = await fetch(url, {
      cache: "no-store",
      headers: { accept: "application/json" },
    });
    if (!response.ok) return null;
    const body: unknown = await response.json();
    if (!body || typeof body !== "object") return null;
    const record = body as { mode?: unknown; gateway_url?: unknown };
    if (record.mode === "gateway") {
      return typeof record.gateway_url === "string" && record.gateway_url
        ? { mode: "gateway", gateway_url: record.gateway_url, resource: "" }
        : null;
    }
    if (record.mode === "static_token" || record.mode === "local") {
      return { mode: record.mode };
    }
    return null;
  } catch {
    return null;
  }
}

/** Vite-dev middleware that reads the desktop's listen.json. Absent in prod. */
export const DESKTOP_LISTEN_PATH = "/__tidebreak/listen";

async function desktopListenInfo(): Promise<ServerInfo | null> {
  if (!import.meta.env.DEV) return null;
  try {
    const response = await fetch(DESKTOP_LISTEN_PATH, { cache: "no-store" });
    if (!response.ok) return null;
    const body: unknown = await response.json();
    if (!body || typeof body !== "object") return null;
    const record = body as { baseUrl?: unknown; token?: unknown };
    const baseUrl =
      typeof record.baseUrl === "string"
        ? record.baseUrl.trim().replace(/\/$/, "")
        : "";
    const token = typeof record.token === "string" ? record.token.trim() : "";
    if (!baseUrl || !token) return null;
    return { baseUrl, token, attachment: "local", gatewayAuth: false };
  } catch {
    return null;
  }
}

function envServerInfo(): ServerInfo | null {
  const baseUrl = import.meta.env.VITE_TIDEBREAK_URL?.trim();
  const token = import.meta.env.VITE_TIDEBREAK_TOKEN?.trim();
  if (!baseUrl || !token) return null;

  return {
    baseUrl: baseUrl.replace(/\/$/, ""),
    token,
    attachment: "local",
    gatewayAuth: false,
  };
}
