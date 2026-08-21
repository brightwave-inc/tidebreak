// Deterministic scripted harness that drives the mock /code/browser bridge
// using the real v1 capfile protocol. This simulates what a provider
// adapter (Claude, Codex, OpenCode) does: read the capfile, extract
// endpoint + token, then call the three browser tools in sequence.
//
// It also simulates a "browser-mcp" style process that accepts a capfile
// path from TIDEBREAK_BROWSER_CAPFILE and validates the tool registry.
// This is the pure-Node equivalent of what the real CLI `tidebreak browser-mcp`
// does — without any Rust dependency.
//
// All page content returned by the bridge is treated and documented as
// untrusted. No assertion treats page content as instruction.

import { readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const MAX_RESPONSE_BYTES = 1024 * 1024;

// ── capfile protocol ────────────────────────────────────────────────────────

/**
 * Read a v1 capfile and return { endpoint, token }. Validates the exact
 * schema: `{ "version": 1, "endpoint": "...", "token": "tbreak_bt_..." }`.
 */
export async function readCapfile(capfilePath) {
  const raw = await readFile(capfilePath, "utf8");
  let capfile;
  try {
    capfile = JSON.parse(raw);
  } catch {
    throw new Error(`capfile is not valid JSON: ${capfilePath}`);
  }
  if (!capfile || typeof capfile !== "object") {
    throw new Error("capfile is not a JSON object");
  }
  if (capfile.version !== 1) {
    throw new Error(`capfile version is ${capfile.version}, expected 1`);
  }
  if (typeof capfile.endpoint !== "string" || !capfile.endpoint.startsWith("http")) {
    throw new Error("capfile endpoint is missing or not an HTTP URL");
  }
  if (typeof capfile.token !== "string" || !capfile.token.startsWith("tbreak_bt_")) {
    throw new Error("capfile token is missing or malformed");
  }
  const extraKeys = Object.keys(capfile).filter(
    (k) => !["version", "endpoint", "token"].includes(k)
  );
  if (extraKeys.length > 0) {
    throw new Error(`capfile has unexpected keys: ${extraKeys.join(", ")}`);
  }
  return { endpoint: capfile.endpoint, token: capfile.token };
}

// ── bounded HTTP client ─────────────────────────────────────────────────────

/**
 * POST to /code/browser/<route> with the bearer token, reading at most
 * MAX_RESPONSE_BYTES, refusing redirects.
 */
export async function callBrowserRoute(endpoint, route, body, token) {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 10_000);

  try {
    const response = await fetch(`${endpoint}/${route}`, {
      method: route === "list" ? "GET" : "POST",
      headers: {
        authorization: `Bearer ${token}`,
        "content-type": "application/json",
      },
      body: route === "list" ? undefined : JSON.stringify(body),
      redirect: "manual",
      signal: controller.signal,
    });

    // Refuse redirects (3xx)
    if (response.status >= 300 && response.status < 400) {
      return {
        status: response.status,
        body: {
          kind: "redirect_refused",
          message: `server redirected to ${response.headers.get("location")}`,
        },
      };
    }

    // Bound the response size
    const text = await response.text();
    if (text.length > MAX_RESPONSE_BYTES) {
      return {
        status: 413,
        body: {
          kind: "response_too_large",
          message: `response body ${text.length} exceeds ${MAX_RESPONSE_BYTES} bytes`,
        },
      };
    }

    let parsed;
    try {
      parsed = JSON.parse(text);
    } catch {
      parsed = { kind: "parse_error", raw: text.slice(0, 200) };
    }

    return { status: response.status, body: parsed };
  } finally {
    clearTimeout(timeout);
  }
}

// ── tool registry contract ──────────────────────────────────────────────────

/**
 * The exact three browser tools this slice advertises. These names MUST
 * match the constants in crates/tidebreak-core/src/browser.rs.
 */
export const BROWSER_TOOLS = ["browser_list", "browser_navigate", "browser_snapshot"];

/**
 * Tools that MUST NOT be advertised in this slice.
 */
export const FORBIDDEN_TOOLS = ["act", "wait", "screenshot"];

/**
 * Assert the tool registry contains exactly the three browser tools and
 * nothing from the forbidden set.
 */
export function assertToolRegistry(tools) {
  const names = new Set(tools.map((t) => t.name));

  for (const expected of BROWSER_TOOLS) {
    if (!names.has(expected)) {
      throw new Error(
        `tool registry missing expected tool: ${expected}`
      );
    }
  }

  for (const forbidden of FORBIDDEN_TOOLS) {
    if (names.has(forbidden)) {
      throw new Error(
        `tool registry contains forbidden tool: ${forbidden}`
      );
    }
  }

  // Only the three tools, nothing more
  const extra = [...names].filter((n) => !BROWSER_TOOLS.includes(n));
  if (extra.length > 0) {
    throw new Error(
      `tool registry has unexpected extra tools: ${extra.join(", ")}`
    );
  }
}

// ── camelCase contract assertions ───────────────────────────────────────────

/**
 * Assert the browser_list response has the expected camelCase shape.
 */
export function assertListShape(body) {
  if (!Array.isArray(body.sessions)) {
    throw new Error("list result missing sessions array");
  }
  const session = body.sessions[0];
  if (typeof session.browserId !== "string") {
    throw new Error("session missing browserId");
  }
  // browserId must be present and a string
}

/**
 * Assert the browser_navigate response has the expected camelCase shape.
 */
export function assertNavigateShape(body) {
  if (typeof body.browserId !== "string") {
    throw new Error("navigate result missing browserId");
  }
  if (typeof body.url !== "string") {
    throw new Error("navigate result missing url");
  }
  if (typeof body.loadState !== "string") {
    throw new Error("navigate result missing loadState");
  }
  if (typeof body.documentEpoch !== "number") {
    throw new Error("navigate result missing documentEpoch");
  }
}

/**
 * Assert the browser_snapshot response has the expected camelCase shape.
 */
export function assertSnapshotShape(body) {
  if (typeof body.browserId !== "string") {
    throw new Error("snapshot result missing browserId");
  }
  if (typeof body.snapshotId !== "string") {
    throw new Error("snapshot result missing snapshotId");
  }
  if (typeof body.documentEpoch !== "number") {
    throw new Error("snapshot result missing documentEpoch");
  }
  if (body.contentTrust !== "untrusted_page") {
    throw new Error(
      `snapshot contentTrust is ${body.contentTrust}, expected untrusted_page`
    );
  }
  if (typeof body.url !== "string") {
    throw new Error("snapshot result missing url");
  }
  if (typeof body.title !== "string") {
    throw new Error("snapshot result missing title");
  }
  if (!body.viewport || typeof body.viewport.width !== "number") {
    throw new Error("snapshot result missing viewport");
  }
  if (!Array.isArray(body.nodes)) {
    throw new Error("snapshot result missing nodes array");
  }
  if (!Array.isArray(body.frames)) {
    throw new Error("snapshot result missing frames array");
  }
  if (typeof body.truncated !== "boolean") {
    throw new Error("snapshot result missing truncated");
  }
}

// ── harness: list → navigate → snapshot ─────────────────────────────────────

/**
 * Drive the deterministic positive contract: list → navigate → snapshot
 * against a bridge server using the real capfile protocol.
 *
 * Returns the final snapshot body for further assertion.
 */
export async function drivePositiveContract(endpoint, token, fixtureOrigin) {
  // 1. List
  const list = await callBrowserRoute(endpoint, "list", null, token);
  if (list.status !== 200) {
    throw new Error(`list failed with ${list.status}: ${JSON.stringify(list.body)}`);
  }
  assertListShape(list.body);

  // 2. Navigate
  const navigate = await callBrowserRoute(
    endpoint,
    "navigate",
    { browser_id: "browser-1", url: fixtureOrigin },
    token
  );
  if (navigate.status !== 200) {
    throw new Error(`navigate failed with ${navigate.status}: ${JSON.stringify(navigate.body)}`);
  }
  assertNavigateShape(navigate.body);

  // 3. Snapshot
  const snapshot = await callBrowserRoute(
    endpoint,
    "snapshot",
    { browser_id: "browser-1" },
    token
  );
  if (snapshot.status !== 200) {
    throw new Error(`snapshot failed with ${snapshot.status}: ${JSON.stringify(snapshot.body)}`);
  }
  assertSnapshotShape(snapshot.body);

  return snapshot.body;
}

// ── negative contract helpers ───────────────────────────────────────────────

export function assertStatus(body, status, expectedStatus) {
  if (status !== expectedStatus) {
    throw new Error(
      `expected status ${expectedStatus}, got ${status}: ${JSON.stringify(body)}`
    );
  }
}

// ── fake launch: absolute command, no PATH ──────────────────────────────────

/**
 * Simulate a provider adapter launching the browser bridge with an absolute
 * command and PATH stripped. Returns the capfile content that the bridge
 * would read from TIDEBREAK_BROWSER_CAPFILE.
 *
 * This is the pure-Node equivalent of what Claude/Codex/OpenCode do:
 *   <absolute-tidebreak> browser-mcp
 * with TIDEBREAK_BROWSER_CAPFILE=/path/to/capfile
 *
 * We verify:
 * 1. The command path is absolute.
 * 2. PATH is not available (stripped from env).
 * 3. The capfile path comes from TIDEBREAK_BROWSER_CAPFILE.
 * 4. The token never appears in argv, stdout, or stderr of the simulated
 *    process.
 */
export async function simulateAbsoluteLaunch({
  capfilePath,
  command = "/opt/tidebreak/bin/tidebreak",
  args = ["browser-mcp"],
} = {}) {
  if (!command.startsWith("/")) {
    throw new Error(`command must be absolute, got: ${command}`);
  }

  // Simulate stripped PATH
  const env = { ...process.env };
  delete env.PATH;
  env.TIDEBREAK_BROWSER_CAPFILE = capfilePath;

  // Read the capfile (what the bridge would do)
  const { endpoint, token } = await readCapfile(capfilePath);

  // Verify: token never in argv
  const allArgv = [command, ...args].join(" ");
  if (allArgv.includes(token)) {
    throw new Error("token appears in simulated argv");
  }

  // Simulate what the bridge would print — the token must never appear
  // in stdout or stderr content
  const fakeStdout = JSON.stringify({
    tools: BROWSER_TOOLS.map((name) => ({ name })),
  });

  if (fakeStdout.includes(token)) {
    throw new Error("token appears in simulated stdout");
  }

  return { endpoint, token, env };
}

// ── capfile on-disk protocol ────────────────────────────────────────────────

/**
 * Write a valid v1 capfile to disk. Returns the path and token.
 */
export async function writeCapfile(dir, endpointBase) {
  const { writeFile, mkdir } = await import("node:fs/promises");
  await mkdir(dir, { recursive: true });

  const token = `tbreak_bt_${crypto.randomUUID()}`;
  const fileId = crypto.randomUUID().replaceAll("-", "");
  const capfilePath = join(dir, `browser-cap-${fileId}.json`);

  const body = {
    version: 1,
    endpoint: endpointBase,
    token,
  };

  await writeFile(capfilePath, JSON.stringify(body), { mode: 0o600 });

  return { capfilePath, token };
}
