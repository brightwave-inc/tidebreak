import { invoke, isTauri } from "@tauri-apps/api/core";
import type { RemoteMachineState, ServerInfo } from "./api";

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
 *
 * Every path outside Tauri reports `local`. Host authority is a property of the
 * native shell, and a browser tab has none of it either way, so nothing is
 * gained by calling those attachments remote.
 */
export async function resolveServerInfo(): Promise<ServerInfo> {
  if (isTauri()) {
    return await invoke<ServerInfo>("server_info");
  }

  const fromEnv = envServerInfo();
  if (fromEnv) return fromEnv;

  const fromDesktop = await desktopListenInfo();
  if (fromDesktop) return fromDesktop;

  throw new Error(
    "No Tidebreak server is reachable from this browser tab. Keep " +
      "`scripts/dev.sh` running (it publishes a listen endpoint), or set " +
      "VITE_TIDEBREAK_URL and VITE_TIDEBREAK_TOKEN for `tidebreak serve`.",
  );
}

/** Which machine this client is attached to, without opening a connection. */
export async function remoteMachineState(): Promise<RemoteMachineState> {
  if (!isTauri()) return { attachment: "local", baseUrl: null };
  return await invoke<RemoteMachineState>("remote_machine_state");
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
      typeof record.baseUrl === "string" ? record.baseUrl.trim().replace(/\/$/, "") : "";
    const token = typeof record.token === "string" ? record.token.trim() : "";
    if (!baseUrl || !token) return null;
    return { baseUrl, token, attachment: "local" };
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
  };
}
