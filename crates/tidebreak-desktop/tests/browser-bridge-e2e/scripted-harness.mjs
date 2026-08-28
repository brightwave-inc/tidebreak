// Deterministic scripted harness that drives the mock /code/browser bridge
// using the real v1 capfile protocol. This simulates what a provider
// adapter (Claude, Codex, OpenCode) does: read the capfile, extract
// endpoint, token, and semantic-action capability, then call the browser tools.
//
// It also simulates a "browser-mcp" style process that accepts a capfile
// path from TIDEBREAK_BROWSER_CAPFILE and validates the tool registry.
// This is the pure-Node equivalent of what the real CLI `tidebreak browser-mcp`
// does — without any Rust dependency.
//
// All page content returned by the bridge is treated and documented as
// untrusted. No assertion treats page content as instruction.

import { readFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";
import { isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const MAX_RESPONSE_BYTES = 1024 * 1024;

// ── capfile protocol ────────────────────────────────────────────────────────

/**
 * Read a v1 capfile and return { endpoint, token, semanticActions }. Validates
 * the exact schema written by the server.
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
  if (typeof capfile.semantic_actions !== "boolean") {
    throw new Error("capfile semantic_actions capability is missing or malformed");
  }
  const extraKeys = Object.keys(capfile).filter(
    (k) => !["version", "endpoint", "token", "semantic_actions"].includes(k)
  );
  if (extraKeys.length > 0) {
    throw new Error(`capfile has unexpected keys: ${extraKeys.join(", ")}`);
  }
  return {
    endpoint: capfile.endpoint,
    token: capfile.token,
    semanticActions: capfile.semantic_actions,
  };
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
 * The exact five browser tools this slice advertises. These names MUST
 * match the constants in crates/tidebreak-core/src/browser.rs.
 */
export const BROWSER_TOOLS = [
  "browser_list",
  "browser_navigate",
  "browser_snapshot",
  "browser_wait",
  "browser_screenshot",
];
export const BROWSER_ACTION_TOOL = "browser_act";

/**
 * Tools that MUST NOT be advertised in this slice. browser_act and all
 * semantic action tools remain intentionally absent while
 * semantic_actions is false.
 */
export const FORBIDDEN_TOOLS = [BROWSER_ACTION_TOOL];

/**
 * Assert the tool registry contains exactly the five browser tools and
 * nothing from the forbidden set.
 */
export function assertToolRegistry(tools, { semanticActions = false } = {}) {
  const names = new Set(tools.map((t) => t.name));
  const expectedTools = semanticActions
    ? [...BROWSER_TOOLS, BROWSER_ACTION_TOOL]
    : BROWSER_TOOLS;

  for (const expected of expectedTools) {
    if (!names.has(expected)) {
      throw new Error(
        `tool registry missing expected tool: ${expected}`
      );
    }
  }

  for (const forbidden of FORBIDDEN_TOOLS) {
    if (!semanticActions && names.has(forbidden)) {
      throw new Error(
        `tool registry contains forbidden tool: ${forbidden}`
      );
    }
  }

  const extra = [...names].filter((n) => !expectedTools.includes(n));
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

/**
 * Assert the browser_wait response has the expected camelCase shape.
 */
export function assertWaitShape(body) {
  if (typeof body.browserId !== "string") {
    throw new Error("wait result missing browserId");
  }
  if (typeof body.status !== "string") {
    throw new Error("wait result missing status");
  }
  if (!["resolved", "timed_out", "stopped"].includes(body.status)) {
    throw new Error(
      `wait status is ${body.status}, expected resolved/timed_out/stopped`
    );
  }
  if (typeof body.message !== "string") {
    throw new Error("wait result missing message");
  }
  if (typeof body.documentEpoch !== "number") {
    throw new Error("wait result missing documentEpoch");
  }
  // url and title are Option<String> — present in this mock but optional on the wire
}

/**
 * Assert the browser_screenshot response has the expected camelCase shape.
 */
export function assertScreenshotShape(body) {
  if (typeof body.browserId !== "string") {
    throw new Error("screenshot result missing browserId");
  }
  if (typeof body.snapshotId !== "string") {
    throw new Error("screenshot result missing snapshotId");
  }
  if (typeof body.documentEpoch !== "number") {
    throw new Error("screenshot result missing documentEpoch");
  }
  if (typeof body.imageBase64 !== "string") {
    throw new Error("screenshot result missing imageBase64");
  }
  if (body.mimeType !== "image/png") {
    throw new Error(
      `screenshot mimeType is ${body.mimeType}, expected image/png`
    );
  }
  // Verify the base64 string is a valid PNG (starts with the PNG signature)
  const decoded = Buffer.from(body.imageBase64, "base64");
  // PNG signature: 0x89504E470D0A1A0A
  if (decoded.length < 8 || decoded[0] !== 0x89 || decoded[1] !== 0x50 || decoded[2] !== 0x4e || decoded[3] !== 0x47) {
    throw new Error("screenshot imageBase64 is not a valid PNG");
  }
}

/**
 * Assert the browser_act response has the expected camelCase shape.
 */
export function assertActShape(body) {
  if (typeof body.browserId !== "string") {
    throw new Error("act result missing browserId");
  }
  if (typeof body.snapshotId !== "string") {
    throw new Error("act result missing snapshotId");
  }
  if (typeof body.documentEpoch !== "number") {
    throw new Error("act result missing documentEpoch");
  }
  if (typeof body.ref !== "string") {
    throw new Error("act result missing ref");
  }
  if (typeof body.action !== "string") {
    throw new Error("act result missing action");
  }
  if (typeof body.status !== "string") {
    throw new Error("act result missing status");
  }
  if (typeof body.message !== "string") {
    throw new Error("act result missing message");
  }
  if (typeof body.requiresResnapshot !== "boolean") {
    throw new Error("act result missing requiresResnapshot");
  }
}

// ── harness: list → navigate → snapshot → wait → screenshot ────────────────

/**
 * Drive the deterministic positive contract: list → navigate → snapshot
 * → wait → screenshot against a bridge server using the real capfile
 * protocol.
 *
 * Returns the final screenshot body for further assertion.
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

  const snapshotId = snapshot.body.snapshotId;
  const documentEpoch = snapshot.body.documentEpoch;

  // 4. Wait (load_state ready — deterministic resolve)
  const wait = await callBrowserRoute(
    endpoint,
    "wait",
    {
      browser_id: "browser-1",
      snapshot_id: snapshotId,
      document_epoch: documentEpoch,
      condition: { kind: "load_state", state: "ready" },
    },
    token
  );
  if (wait.status !== 200) {
    throw new Error(`wait failed with ${wait.status}: ${JSON.stringify(wait.body)}`);
  }
  assertWaitShape(wait.body);

  // 5. Screenshot
  const screenshot = await callBrowserRoute(
    endpoint,
    "screenshot",
    {
      browser_id: "browser-1",
      snapshot_id: snapshotId,
      document_epoch: documentEpoch,
    },
    token
  );
  if (screenshot.status !== 200) {
    throw new Error(`screenshot failed with ${screenshot.status}: ${JSON.stringify(screenshot.body)}`);
  }
  assertScreenshotShape(screenshot.body);

  return screenshot.body;
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
 * Launch a real child process through an absolute executable with PATH absent.
 * The probe reads the capfile only through TIDEBREAK_BROWSER_CAPFILE and emits
 * the same non-secret tool roster a browser MCP bridge exposes.
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
  command = process.execPath,
  args = [fileURLToPath(import.meta.url), "browser-mcp-probe"],
} = {}) {
  if (!isAbsolute(command)) {
    throw new Error(`command must be absolute, got: ${command}`);
  }

  // Preserve the normal process environment except for every spelling of
  // PATH. Windows treats environment keys case-insensitively.
  const env = { ...process.env };
  for (const key of Object.keys(env)) {
    if (key.toLowerCase() === "path") delete env[key];
  }
  env.TIDEBREAK_BROWSER_CAPFILE = capfilePath;

  // Read once in the parent only to verify that no secret escaped through the
  // child process boundary.
  const { endpoint, token, semanticActions } = await readCapfile(capfilePath);
  const result = await runChild(command, args, env);
  const allArgv = [command, ...args].join(" ");
  if (
    outputContainsSecret(allArgv, token) ||
    outputContainsSecret(result.stdout, token) ||
    outputContainsSecret(result.stderr, token)
  ) {
    throw new Error("token escaped through child process argv or output");
  }
  if (outputContainsSecret(allArgv, capfilePath)) {
    throw new Error("capfile path escaped into child process argv");
  }
  if (
    outputContainsSecret(result.stdout, capfilePath) ||
    outputContainsSecret(result.stderr, capfilePath)
  ) {
    throw new Error("capfile path escaped through child process output");
  }
  if (result.code !== 0) {
    throw new Error(`absolute launch failed (${result.code}): ${result.stderr}`);
  }

  const output = JSON.parse(result.stdout);
  assertToolRegistry(output.tools, { semanticActions });
  if (output.endpoint !== endpoint) {
    throw new Error("child read a different capfile endpoint");
  }

  return {
    endpoint,
    token,
    command,
    argv: [command, ...args],
    stdout: result.stdout,
    stderr: result.stderr,
    envHasPath: Object.keys(env).some((key) => key.toLowerCase() === "path"),
  };
}

function outputContainsSecret(output, secret) {
  const jsonEscaped = JSON.stringify(secret).slice(1, -1);
  return output.includes(secret) || output.includes(jsonEscaped);
}

function runChild(command, args, env) {
  return new Promise((resolveResult, reject) => {
    const child = spawn(command, args, {
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.once("error", reject);
    child.once("close", (code) => {
      resolveResult({ code, stdout, stderr });
    });
  });
}

// ── capfile on-disk protocol ────────────────────────────────────────────────

/**
 * Write a valid v1 capfile to disk. Returns the path and token.
 */
export async function writeCapfile(dir, endpointBase, { semanticActions = false } = {}) {
  const { writeFile, mkdir } = await import("node:fs/promises");
  await mkdir(dir, { recursive: true });

  const token = `tbreak_bt_${crypto.randomUUID()}`;
  const fileId = crypto.randomUUID().replaceAll("-", "");
  const capfilePath = join(dir, `browser-cap-${fileId}.json`);

  const body = {
    version: 1,
    endpoint: endpointBase,
    token,
    semantic_actions: semanticActions,
  };

  await writeFile(capfilePath, JSON.stringify(body), { mode: 0o600 });

  return { capfilePath, token };
}

const invokedPath = process.argv[1] ? resolve(process.argv[1]) : null;
if (
  invokedPath === resolve(fileURLToPath(import.meta.url)) &&
  process.argv[2] === "browser-mcp-probe"
) {
  try {
    const capfilePath = process.env.TIDEBREAK_BROWSER_CAPFILE;
    if (!capfilePath) throw new Error("TIDEBREAK_BROWSER_CAPFILE is required");
    const { endpoint, semanticActions } = await readCapfile(capfilePath);
    const tools = semanticActions
      ? [...BROWSER_TOOLS, BROWSER_ACTION_TOOL]
      : BROWSER_TOOLS;
    process.stdout.write(
      JSON.stringify({
        endpoint,
        tools: tools.map((name) => ({ name })),
      })
    );
  } catch (error) {
    process.stderr.write(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}
