import { invoke } from "@tauri-apps/api/core";
import type { ServerInfo } from "./api";

/**
 * Resolve how the UI reaches the local API.
 *
 * - Inside Tauri: `server_info` from the host (in-process server).
 * - In a plain browser (`pnpm --dir ui dev`): `VITE_OPENWAVE_URL` +
 *   `VITE_OPENWAVE_TOKEN` (+ optional `VITE_OPENWAVE_WORKSPACE`), so the same
 *   React app can be exercised against `openwave serve`.
 */
export async function resolveServerInfo(): Promise<ServerInfo> {
  const fromEnv = envServerInfo();
  if (fromEnv) return fromEnv;

  try {
    return await invoke<ServerInfo>("server_info");
  } catch (err) {
    const hint =
      "Set VITE_OPENWAVE_URL and VITE_OPENWAVE_TOKEN (from `openwave serve`) " +
      "to run the UI in a browser, or launch via `cargo tauri dev`.";
    throw new Error(`${String(err)}\n\n${hint}`);
  }
}

function envServerInfo(): ServerInfo | null {
  const baseUrl = import.meta.env.VITE_OPENWAVE_URL?.trim();
  const token = import.meta.env.VITE_OPENWAVE_TOKEN?.trim();
  if (!baseUrl || !token) return null;

  const workspaceDir =
    import.meta.env.VITE_OPENWAVE_WORKSPACE?.trim() ||
    defaultWorkspaceFallback();

  return {
    baseUrl: baseUrl.replace(/\/$/, ""),
    token,
    workspaceDir,
  };
}

function defaultWorkspaceFallback(): string {
  // Absolute path required by the API; browser-dev users should set
  // VITE_OPENWAVE_WORKSPACE to a real directory on the machine running serve.
  return "/tmp/openwave-workspace";
}
