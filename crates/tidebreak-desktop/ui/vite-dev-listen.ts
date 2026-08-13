import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import type { Plugin } from "vite";

/** Same identifier the debug desktop writes `listen.json` under. */
export const DEV_APP_IDENTIFIER = "io.brightwave.tidebreak.dev";

export const LISTEN_DEV_PATH = "/__tidebreak/listen";

export type ListenPayload = {
  baseUrl: string;
  token: string;
};

/**
 * Profile directories the debug desktop (or `TIDEBREAK_DATA_DIR`) may have
 * published a listen endpoint into.
 */
export function candidateListenFiles(
  env: NodeJS.ProcessEnv = process.env,
  home: string = os.homedir(),
): string[] {
  const files: string[] = [];
  const override = env.TIDEBREAK_DATA_DIR?.trim();
  if (override) files.push(path.join(override, "listen.json"));

  if (process.platform === "darwin") {
    files.push(
      path.join(home, "Library", "Application Support", DEV_APP_IDENTIFIER, "listen.json"),
    );
  } else if (process.platform === "win32") {
    const appData = env.APPDATA?.trim() || path.join(home, "AppData", "Roaming");
    files.push(path.join(appData, DEV_APP_IDENTIFIER, "listen.json"));
  } else {
    const dataHome = env.XDG_DATA_HOME?.trim() || path.join(home, ".local", "share");
    files.push(path.join(dataHome, DEV_APP_IDENTIFIER, "listen.json"));
  }
  return files;
}

export function parseListenFile(contents: string): ListenPayload | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(contents);
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== "object") return null;
  const record = parsed as { base_url?: unknown; token?: unknown };
  const baseUrl =
    typeof record.base_url === "string" ? record.base_url.trim().replace(/\/$/, "") : "";
  const token = typeof record.token === "string" ? record.token.trim() : "";
  if (!baseUrl || !token) return null;
  return { baseUrl, token };
}

export function readDevListenPayload(
  env: NodeJS.ProcessEnv = process.env,
  home: string = os.homedir(),
): ListenPayload | null {
  for (const file of candidateListenFiles(env, home)) {
    try {
      const payload = parseListenFile(fs.readFileSync(file, "utf8"));
      if (payload) return payload;
    } catch {
      // Missing or unreadable — try the next candidate.
    }
  }
  return null;
}

/**
 * Dev-only: expose the running desktop/`serve` listen endpoint so a browser
 * tab on the Vite origin can attach without baking the bearer into env.
 */
export function tidebreakDevListenPlugin(): Plugin {
  return {
    name: "tidebreak-dev-listen",
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        if (req.url?.split("?")[0] !== LISTEN_DEV_PATH) {
          next();
          return;
        }
        if (req.method !== "GET" && req.method !== "HEAD") {
          res.statusCode = 405;
          res.end();
          return;
        }
        const payload = readDevListenPayload();
        if (!payload) {
          res.statusCode = 404;
          res.setHeader("Content-Type", "application/json");
          res.end(
            JSON.stringify({
              error:
                "No Tidebreak server is publishing a listen endpoint. Keep `scripts/dev.sh` running, or set VITE_TIDEBREAK_URL and VITE_TIDEBREAK_TOKEN.",
            }),
          );
          return;
        }
        res.statusCode = 200;
        res.setHeader("Content-Type", "application/json");
        res.setHeader("Cache-Control", "no-store");
        res.end(req.method === "HEAD" ? "" : JSON.stringify(payload));
      });
    },
  };
}
