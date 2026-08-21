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

// ── validation constants (match crates/tidebreak-core/src/browser.rs) ───────

const MAX_BROWSER_ID_CHARS = 80;
const MAX_BROWSER_URL_CHARS = 8_192;
const DEFAULT_BROWSER_WAIT_TIMEOUT_MS = 5_000;
const MAX_BROWSER_WAIT_TIMEOUT_MS = 30_000;
const MAX_BROWSER_SCREENSHOT_DIMENSION = 4_096;
const MAX_WAIT_TEXT_CHARS = 512;

const BROWSER_ID_RE = /^[A-Za-z0-9_-]{1,80}$/;

function validBrowserId(id) {
  return typeof id === "string" && BROWSER_ID_RE.test(id);
}

function validWaitCondition(condition) {
  if (!condition || typeof condition !== "object") return false;
  const kind = condition.kind;
  switch (kind) {
    case "url_changed":
      return true;
    case "load_state":
      // is_well_formed always returns true for LoadState; the state value
      // validity is a deserialization concern (handled as 400 above).
      return true;
    case "text_present":
    case "text_absent":
      return (
        typeof condition.text === "string" &&
        [...condition.text].length <= MAX_WAIT_TEXT_CHARS
      );
    default:
      return false;
  }
}

function validTimeoutMs(timeoutMs) {
  if (timeoutMs === undefined || timeoutMs === null) return true;
  return (
    typeof timeoutMs === "number" &&
    Number.isInteger(timeoutMs) &&
    timeoutMs >= 100 &&
    timeoutMs <= MAX_BROWSER_WAIT_TIMEOUT_MS
  );
}

function validScreenshotDimension(value, allowZero) {
  if (value === undefined || value === null) return true;
  if (typeof value !== "number" || !Number.isInteger(value)) return false;
  if (allowZero && value === 0) return true;
  return value >= 1 && value <= MAX_BROWSER_SCREENSHOT_DIMENSION;
}

// ── deterministic small valid PNG ───────────────────────────────────────────

/**
 * A deterministic 1×1 transparent PNG, base64-encoded. This is the smallest
 * valid PNG (67 bytes raw). The screenshot route returns this so the test can
 * verify the wire shape without depending on a native image capture.
 */
const DETERMINISTIC_PNG_BASE64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

// ── server ──────────────────────────────────────────────────────────────────

/**
 * Start the mock bridge server.
 *
 * The returned `endpoint` is the full `/code/browser` base URL —
 * callers append just `list`, `navigate`, `snapshot`, `wait`, or `screenshot`.
 * This matches the real capfile `endpoint` field exactly.
 *
 * Tokens are strings returned by `tokenRegistry.issue()`. The server
 * validates token presence and liveness but does not derive
 * workspace/session identity from the token.
 *
 * Options:
 * - staleSnapshot: if true, snapshot/wait/screenshot return 409 stale_browser_target
 * - hiddenBrowser: if true, list returns visible=false
 * - stoppedControl: if true, list returns controller.halted=true, wait returns stopped
 * - missingRuntime: if true, all routes return 501 after auth+validation
 * - waitOutcome: "resolved" | "timed_out" | "stopped" — controls wait result status
 * - staleInstance: if true, wait/screenshot return a 409 with instance-replaced semantics
 */
export async function startBridgeServer({
  host = "127.0.0.1",
  port = 0,
  fixtureOrigin,
  staleSnapshot = false,
  hiddenBrowser = false,
  stoppedControl = false,
  missingRuntime = false,
  waitOutcome = "resolved",
  staleInstance = false,
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

  // The current snapshot/document epoch. Navigate increments it.
  let currentDocumentEpoch = 2;
  let currentSnapshotId = `snapshot-${randomUUID().slice(0, 8)}`;

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

      const route = url.pathname.slice("/code/browser/".length);

      if (route === "list" && request.method === "GET") {
        if (missingRuntime) {
          errJson(
            response,
            501,
            "not_implemented",
            "this server has no in-app browser runtime"
          );
          return;
        }
        const controller = defaultController();
        if (stoppedControl) controller.halted = true;

        json(response, {
          sessions: hiddenBrowser
            ? []
            : [
                {
                  browserId: "browser-1",
                  url: fixtureOrigin,
                  title: "Agent browser fixture",
                  loadState: "ready",
                  visible: true,
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

        if (!/^[A-Za-z0-9_-]{1,80}$/.test(browser_id) || navUrl.length > 8192) {
          errJson(
            response,
            422,
            "invalid_browser_arguments",
            "browser arguments are not well-formed"
          );
          return;
        }

        if (missingRuntime) {
          errJson(
            response,
            501,
            "not_implemented",
            "this server has no in-app browser runtime"
          );
          return;
        }

        // Cross-workspace / unknown browser id → 404
        if (browser_id !== "browser-1") {
          errJson(response, 404, "not_found", `browser ${browser_id} not found`);
          return;
        }

        // Navigate increments the document epoch and generates a new snapshot id
        currentDocumentEpoch += 1;
        currentSnapshotId = `snapshot-${randomUUID().slice(0, 8)}`;

        json(response, {
          browserId: "browser-1",
          url: navUrl,
          loadState: "loading",
          documentEpoch: currentDocumentEpoch,
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

        if (!/^[A-Za-z0-9_-]{1,80}$/.test(browser_id)) {
          errJson(
            response,
            422,
            "invalid_browser_arguments",
            "browser arguments are not well-formed"
          );
          return;
        }

        if (missingRuntime) {
          errJson(
            response,
            501,
            "not_implemented",
            "this server has no in-app browser runtime"
          );
          return;
        }

        // Cross-workspace / unknown browser id → 404
        if (browser_id !== "browser-1") {
          errJson(response, 404, "not_found", `browser ${browser_id} not found`);
          return;
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

        // Refresh the snapshot id on each successful snapshot
        currentSnapshotId = `snapshot-${randomUUID().slice(0, 8)}`;

        json(response, {
          browserId: "browser-1",
          snapshotId: currentSnapshotId,
          documentEpoch: currentDocumentEpoch,
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

      // ── POST /wait ──────────────────────────────────────────────────────

      if (route === "wait" && request.method === "POST") {
        const body = await readBody(request);

        if (!body || typeof body !== "object") {
          errJson(response, 400, "bad_request", "invalid JSON body");
          return;
        }

        // Deny unknown fields (matches #[serde(deny_unknown_fields)])
        const allowed = new Set([
          "browser_id",
          "snapshot_id",
          "document_epoch",
          "condition",
          "timeout_ms",
        ]);
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

        const { browser_id, snapshot_id, document_epoch, condition, timeout_ms } = body;

        // Missing required fields → 400
        if (typeof browser_id !== "string") {
          errJson(response, 400, "bad_request", "missing browser_id");
          return;
        }
        if (typeof snapshot_id !== "string") {
          errJson(response, 400, "bad_request", "missing snapshot_id");
          return;
        }
        if (typeof document_epoch !== "number" || !Number.isInteger(document_epoch) || document_epoch < 0) {
          errJson(response, 400, "bad_request", "missing or invalid document_epoch");
          return;
        }
        if (!condition || typeof condition !== "object") {
          errJson(response, 400, "bad_request", "missing or invalid condition");
          return;
        }

        // Validate browser_id format → 422
        if (!validBrowserId(browser_id)) {
          errJson(
            response,
            422,
            "invalid_browser_arguments",
            "browser arguments are not well-formed"
          );
          return;
        }

        // Condition deserialization: missing kind or unknown kind → 400
        // (matches serde rejection of the tagged enum)
        const condKind = condition.kind;
        if (typeof condKind !== "string") {
          errJson(response, 400, "bad_request", "condition missing kind tag");
          return;
        }
        if (!["url_changed", "load_state", "text_present", "text_absent"].includes(condKind)) {
          errJson(response, 400, "bad_request", `unknown condition kind: ${condKind}`);
          return;
        }
        // Variant field deserialization: missing required variant fields → 400
        if (condKind === "load_state") {
          if (typeof condition.state !== "string") {
            errJson(response, 400, "bad_request", "load_state condition missing state");
            return;
          }
          // BrowserLoadState is a snake_case enum: idle, loading, ready, failed
          if (!["idle", "loading", "ready", "failed"].includes(condition.state)) {
            errJson(response, 400, "bad_request", `unknown load_state: ${condition.state}`);
            return;
          }
        }
        if ((condKind === "text_present" || condKind === "text_absent") && typeof condition.text !== "string") {
          errJson(response, 400, "bad_request", `${condKind} condition missing text`);
          return;
        }

        // Condition is_well_formed check (post-deserialization) → 422
        if (!validWaitCondition(condition)) {
          errJson(
            response,
            422,
            "invalid_browser_arguments",
            "browser arguments are not well-formed"
          );
          return;
        }

        // Validate timeout_ms bounds → 422
        if (!validTimeoutMs(timeout_ms)) {
          errJson(
            response,
            422,
            "invalid_browser_arguments",
            "browser arguments are not well-formed"
          );
          return;
        }

        if (missingRuntime) {
          errJson(
            response,
            501,
            "not_implemented",
            "this server has no in-app browser runtime"
          );
          return;
        }

        // Cross-workspace / unknown browser id → 404
        if (browser_id !== "browser-1") {
          errJson(response, 404, "not_found", `browser ${browser_id} not found`);
          return;
        }

        // Hidden targets are not addressable through an observation
        // capability, matching BrowserRegistry::list_for_capability and the
        // desktop adapter's fail-closed mapping.
        if (hiddenBrowser) {
          errJson(response, 404, "not_found", `browser ${browser_id} not found`);
          return;
        }

        // A Stop that already happened refuses a new operation. The separate
        // waitOutcome="stopped" fixture models Stop racing an in-flight wait.
        if (stoppedControl) {
          errJson(response, 403, "forbidden", "browser control was stopped by the user");
          return;
        }

        // Snapshot identity and epoch are authoritative even without an
        // explicitly forced stale fixture.
        if (
          staleSnapshot ||
          document_epoch !== currentDocumentEpoch ||
          snapshot_id !== currentSnapshotId
        ) {
          errJson(
            response,
            409,
            "stale_browser_target",
            "the page changed since that snapshot; take a new browser_snapshot"
          );
          return;
        }

        // Stale instance (session replaced) → 409
        if (staleInstance) {
          errJson(
            response,
            409,
            "stale_browser_target",
            "browser session was replaced while waiting"
          );
          return;
        }

        // Deterministic wait outcome
        let status;
        let message;
        if (waitOutcome === "timed_out") {
          status = "timed_out";
          const effectiveTimeout = timeout_ms ?? DEFAULT_BROWSER_WAIT_TIMEOUT_MS;
          message = `Wait timed out after ${effectiveTimeout} ms.`;
        } else if (waitOutcome === "stopped") {
          status = "stopped";
          message = "Browser control was stopped by the user.";
        } else {
          status = "resolved";
          message = "Wait condition satisfied.";
        }

        json(response, {
          browserId: "browser-1",
          status,
          message,
          documentEpoch: currentDocumentEpoch,
          url: fixtureOrigin,
          title: "Agent browser fixture",
        });
        return;
      }

      // ── POST /screenshot ────────────────────────────────────────────────

      if (route === "screenshot" && request.method === "POST") {
        const body = await readBody(request);

        if (!body || typeof body !== "object") {
          errJson(response, 400, "bad_request", "invalid JSON body");
          return;
        }

        // Deny unknown fields (matches #[serde(deny_unknown_fields)])
        const allowed = new Set([
          "browser_id",
          "snapshot_id",
          "document_epoch",
          "max_width",
          "max_height",
        ]);
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

        const { browser_id, snapshot_id, document_epoch, max_width, max_height } = body;

        // Missing required fields → 400
        if (typeof browser_id !== "string") {
          errJson(response, 400, "bad_request", "missing browser_id");
          return;
        }
        if (typeof snapshot_id !== "string") {
          errJson(response, 400, "bad_request", "missing snapshot_id");
          return;
        }
        if (typeof document_epoch !== "number" || !Number.isInteger(document_epoch) || document_epoch < 0) {
          errJson(response, 400, "bad_request", "missing or invalid document_epoch");
          return;
        }

        // Validate browser_id format → 422
        if (!validBrowserId(browser_id)) {
          errJson(
            response,
            422,
            "invalid_browser_arguments",
            "browser arguments are not well-formed"
          );
          return;
        }

        // Validate dimension bounds → 422
        if (!validScreenshotDimension(max_width, false)) {
          errJson(
            response,
            422,
            "invalid_browser_arguments",
            "browser arguments are not well-formed"
          );
          return;
        }
        if (!validScreenshotDimension(max_height, true)) {
          errJson(
            response,
            422,
            "invalid_browser_arguments",
            "browser arguments are not well-formed"
          );
          return;
        }

        if (missingRuntime) {
          errJson(
            response,
            501,
            "not_implemented",
            "this server has no in-app browser runtime"
          );
          return;
        }

        // Cross-workspace / unknown browser id → 404
        if (browser_id !== "browser-1") {
          errJson(response, 404, "not_found", `browser ${browser_id} not found`);
          return;
        }

        if (hiddenBrowser) {
          errJson(response, 404, "not_found", `browser ${browser_id} not found`);
          return;
        }

        if (stoppedControl) {
          errJson(response, 403, "forbidden", "browser control was stopped by the user");
          return;
        }

        // Screenshot publication must echo a host-issued snapshot for the
        // exact live document generation.
        if (
          staleSnapshot ||
          document_epoch !== currentDocumentEpoch ||
          snapshot_id !== currentSnapshotId
        ) {
          errJson(
            response,
            409,
            "stale_browser_target",
            "the page changed since that snapshot; take a new browser_snapshot"
          );
          return;
        }

        // Stale instance (session replaced) → 409
        if (staleInstance) {
          errJson(
            response,
            409,
            "stale_browser_target",
            "browser session was replaced while capturing the screenshot"
          );
          return;
        }

        json(response, {
          browserId: "browser-1",
          snapshotId: snapshot_id,
          documentEpoch: currentDocumentEpoch,
          imageBase64: DETERMINISTIC_PNG_BASE64,
          mimeType: "image/png",
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
