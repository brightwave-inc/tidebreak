// Deterministic loopback-only mock of the Tidebreak /code/browser/* HTTP
// routes. This is a pure-Node server with no external dependencies. It is
// NOT a real BrowserRuntime — it does not touch a native browser engine.
// It implements only the HTTP contract that the real CLI/MCP bridge speaks
// against the desktop server, using the exact route shapes, status codes,
// and error kinds from the current route layer.
//
// Every response body matches the camelCase core wire schema; see
// crates/tidebreak-core/src/browser.rs for the authoritative types.

import { createServer } from "node:http";
import { randomUUID } from "node:crypto";

const MAX_BODY_BYTES = 1024 * 1024;

// ── helpers ────────────────────────────────────────────────────────────────

function json(response, value, status = 200) {
  const body = JSON.stringify(value);
  response.writeHead(status, {
    "cache-control": "no-store",
    "content-type": "application/json; charset=utf-8",
    "x-content-type-options": "nosniff",
    "content-length": Buffer.byteLength(body),
  });
  response.end(body);
}

function errJson(response, status, kind, message) {
  json(response, { kind, message }, status);
}

async function readBody(request) {
  const chunks = [];
  let size = 0;
  for await (const chunk of request) {
    size += chunk.length;
    if (size > MAX_BODY_BYTES) return null;
    chunks.push(chunk);
  }
  try {
    return JSON.parse(Buffer.concat(chunks).toString("utf8"));
  } catch {
    return null;
  }
}

function bearerToken(headers) {
  const auth = headers["authorization"];
  if (!auth) return null;
  const m = /^Bearer\s+(.+)$/i.exec(auth);
  return m ? m[1] : null;
}

function listen(server, host, port) {
  return new Promise((resolveListen, reject) => {
    server.once("error", reject);
    server.listen(port, host, () => {
      server.off("error", reject);
      resolveListen(server.address());
    });
  });
}

export function close(server) {
  return new Promise((resolveClose, reject) => {
    server.close((error) => (error ? reject(error) : resolveClose()));
  });
}

// ── deterministic browser state ─────────────────────────────────────────────

function defaultEngine() {
  return {
    name: "wk_web_view",
    capabilities: {
      lifecycle: true,
      persistentProfile: true,
      semanticSnapshot: true,
      semanticActions: false,
      screenshot: true,
      crossOriginFrames: false,
      profileReset: true,
    },
  };
}

function defaultController() {
  return {
    kind: "human",
    halted: false,
    takeoverRequired: false,
  };
}

// ── server ──────────────────────────────────────────────────────────────────

/**
 * Start the mock bridge server.
 *
 * The returned `endpoint` is the full `/code/browser` base URL —
 * callers append just `list`, `navigate`, or `snapshot`. This matches
 * the real capfile `endpoint` field exactly.
 *
 * Tokens are strings returned by `tokenRegistry.issue()`. The server
 * validates token presence and liveness but does not derive
 * workspace/session identity from the token.
 */
export async function startBridgeServer({
  host = "127.0.0.1",
  port = 0,
  fixtureOrigin,
  staleSnapshot = false,
  hiddenBrowser = false,
  stoppedControl = false,
  missingRuntime = false,
} = {}) {
  if (!fixtureOrigin) {
    throw new Error("fixtureOrigin is required");
  }

  // Simple in-memory token registry: token → { valid, ended }
  const tokenRegistry = {
    entries: new Map(),
    issue() {
      const token = `tbreak_bt_${randomUUID()}`;
      this.entries.set(token, { valid: true, ended: false });
      return token;
    },
    revoke(token) {
      const entry = this.entries.get(token);
      if (entry) entry.valid = false;
    },
    end(token) {
      const entry = this.entries.get(token);
      if (entry) entry.ended = true;
    },
    isValid(token) {
      const entry = this.entries.get(token);
      return entry ? entry.valid : null;
    },
    isEnded(token) {
      const entry = this.entries.get(token);
      return entry ? entry.ended : false;
    },
  };

  const server = createServer(async (request, response) => {
    try {
      const url = new URL(request.url ?? "/", "http://fixture.invalid");

      // All /code/browser/* routes
      if (!url.pathname.startsWith("/code/browser/")) {
        json(response, { error: "not_found" }, 404);
        return;
      }

      const token = bearerToken(request.headers);

      // Missing token → 401
      if (!token) {
        errJson(response, 401, "unauthorized", "missing browser capability token");
        return;
      }

      // Unknown/bad or revoked token → 401
      const validity = tokenRegistry.isValid(token);
      if (validity === null || validity === false) {
        errJson(response, 401, "unauthorized", "unknown or revoked browser capability token");
        return;
      }

      // Ended session → 403
      if (tokenRegistry.isEnded(token)) {
        errJson(response, 403, "forbidden", "the browser session has ended");
        return;
      }

      // Match the real route refusal ladder: authorize the capability first,
      // then reveal whether a native runtime is attached.
      if (missingRuntime) {
        errJson(
          response,
          501,
          "not_implemented",
          "this server has no in-app browser runtime"
        );
        return;
      }

      const route = url.pathname.slice("/code/browser/".length);

      if (route === "list" && request.method === "GET") {
        const controller = defaultController();
        if (stoppedControl) controller.halted = true;

        json(response, {
          sessions: [
            {
              browserId: "browser-1",
              url: fixtureOrigin,
              title: "Agent browser fixture",
              loadState: "ready",
              visible: !hiddenBrowser,
              engine: defaultEngine(),
              controller,
            },
          ],
        });
        return;
      }

      if (route === "navigate" && request.method === "POST") {
        const body = await readBody(request);

        // Malformed body → 400
        if (!body || typeof body !== "object") {
          errJson(response, 400, "bad_request", "invalid JSON body");
          return;
        }

        // Deny unknown fields (matches #[serde(deny_unknown_fields)])
        const allowed = new Set(["browser_id", "url"]);
        for (const key of Object.keys(body)) {
          if (!allowed.has(key)) {
            errJson(
              response,
              400,
              "invalid_browser_arguments",
              `unknown field \`${key}\``
            );
            return;
          }
        }

        const { browser_id, url: navUrl } = body;

        // Missing required fields → 400
        if (typeof browser_id !== "string" || typeof navUrl !== "string") {
          errJson(response, 400, "bad_request", "missing required fields");
          return;
        }

        // Cross-workspace / unknown browser id → 404
        if (browser_id !== "browser-1") {
          errJson(response, 404, "not_found", `browser ${browser_id} not found`);
          return;
        }

        // Invalid URL → 422
        let parsedNavUrl;
        try {
          parsedNavUrl = new URL(navUrl);
        } catch {
          errJson(
            response,
            422,
            "invalid_browser_arguments",
            "browser arguments are not well-formed"
          );
          return;
        }

        if (!["http:", "https:"].includes(parsedNavUrl.protocol)) {
          errJson(
            response,
            422,
            "invalid_browser_arguments",
            "browser arguments are not well-formed"
          );
          return;
        }

        if (parsedNavUrl.username || parsedNavUrl.password) {
          errJson(
            response,
            422,
            "invalid_browser_arguments",
            "browser arguments are not well-formed"
          );
          return;
        }

        json(response, {
          browserId: "browser-1",
          url: navUrl,
          loadState: "loading",
          documentEpoch: 3,
        });
        return;
      }

      if (route === "snapshot" && request.method === "POST") {
        const body = await readBody(request);

        if (!body || typeof body !== "object") {
          errJson(response, 400, "bad_request", "invalid JSON body");
          return;
        }

        // Deny unknown fields
        const allowed = new Set(["browser_id", "max_nodes"]);
        for (const key of Object.keys(body)) {
          if (!allowed.has(key)) {
            errJson(
              response,
              400,
              "invalid_browser_arguments",
              `unknown field \`${key}\``
            );
            return;
          }
        }

        const { browser_id, max_nodes } = body;

        if (typeof browser_id !== "string") {
          errJson(response, 400, "bad_request", "missing browser_id");
          return;
        }

        // Cross-workspace / unknown browser id → 404
        if (browser_id !== "browser-1") {
          errJson(response, 404, "not_found", `browser ${browser_id} not found`);
          return;
        }

        // Validate max_nodes bounds
        if (max_nodes !== undefined) {
          if (
            typeof max_nodes !== "number" ||
            !Number.isInteger(max_nodes) ||
            max_nodes < 1 ||
            max_nodes > 500
          ) {
            errJson(
              response,
              422,
              "invalid_browser_arguments",
              "browser arguments are not well-formed"
            );
            return;
          }
        }

        // Stale epoch → 409
        if (staleSnapshot) {
          errJson(
            response,
            409,
            "stale_browser_target",
            "the page changed since that snapshot; take a new browser_snapshot"
          );
          return;
        }

        json(response, {
          browserId: "browser-1",
          snapshotId: `snapshot-${randomUUID().slice(0, 8)}`,
          documentEpoch: 2,
          contentTrust: "untrusted_page",
          url: fixtureOrigin,
          title: "Agent browser fixture",
          viewport: { width: 1024, height: 768, scrollX: 0, scrollY: 0 },
          nodes: [
            {
              kind: "interactive",
              ref: "@e1",
              tag: "a",
              role: "link",
              name: "Home",
              frame: "top",
              text: "Home",
              href: `${fixtureOrigin}/?view=home`,
              disabled: false,
              sensitive: false,
              actions: ["click"],
              bounds: { x: 48, y: 132, width: 60, height: 36 },
            },
            {
              kind: "interactive",
              ref: "@e2",
              tag: "button",
              role: "button",
              name: "Add item",
              frame: "top",
              text: "Add item",
              disabled: false,
              sensitive: false,
              actions: ["click"],
              bounds: { x: 48, y: 220, width: 90, height: 36 },
            },
            {
              kind: "content",
              tag: "h1",
              role: "heading",
              name: "Agent browser fixture",
              frame: "top",
              text: "Agent browser fixture",
              disabled: false,
              sensitive: false,
              actions: [],
              bounds: { x: 48, y: 80, width: 500, height: 48 },
            },
          ],
          frames: [],
          truncated: false,
        });
        return;
      }

      // Unknown route within /code/browser/
      json(response, { error: "not_found" }, 404);
    } catch {
      if (!response.headersSent) {
        errJson(response, 500, "internal_error", "unexpected server error");
      }
    }
  });

  // Reject non-loopback binds
  const address = await listen(server, host, port);
  if (!address || typeof address === "string") {
    await close(server);
    throw new Error("bridge server did not bind a TCP address");
  }

  const addr = address.address;
  if (addr !== "127.0.0.1" && addr !== "::1" && addr !== "localhost") {
    await close(server);
    throw new Error(
      `bridge server must bind loopback only, got ${addr}`
    );
  }

  // Endpoint is the full /code/browser base — matches the real capfile
  // endpoint field exactly.
  return {
    endpoint: `http://${addr}:${address.port}/code/browser`,
    tokenRegistry,
    server,
  };
}
