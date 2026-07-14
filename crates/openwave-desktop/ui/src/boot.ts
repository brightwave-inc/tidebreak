import { invoke, isTauri } from "@tauri-apps/api/core";
import type { ServerInfo } from "./api";

/**
 * Resolve how the UI reaches the local API.
 *
 * - Inside Tauri: `server_info` from the host (in-process server).
 * - In a plain browser (`pnpm --dir ui dev`): `VITE_OPENWAVE_URL` +
 *   `VITE_OPENWAVE_TOKEN` + explicit `VITE_OPENWAVE_SCRATCH`, so the same
 *   React app can be exercised against `openwave serve`.
 */
export async function resolveServerInfo(): Promise<ServerInfo> {
  if (isTauri()) {
    return await invoke<ServerInfo>("server_info");
  }

  const fromEnv = envServerInfo();
  if (fromEnv) return fromEnv;

  throw new Error(
    "Set VITE_OPENWAVE_URL, VITE_OPENWAVE_TOKEN, and VITE_OPENWAVE_SCRATCH " +
      "(for `openwave serve`) " +
      "to run the UI in a browser, or launch via `cargo tauri dev`.",
  );
}

function envServerInfo(): ServerInfo | null {
  const baseUrl = import.meta.env.VITE_OPENWAVE_URL?.trim();
  const token = import.meta.env.VITE_OPENWAVE_TOKEN?.trim();
  if (!baseUrl || !token) return null;

  const scratchDir = import.meta.env.VITE_OPENWAVE_SCRATCH?.trim();
  if (!scratchDir) return null;

  return {
    baseUrl: baseUrl.replace(/\/$/, ""),
    token,
    scratchDir,
  };
}
