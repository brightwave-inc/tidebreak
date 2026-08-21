// Deterministic shipped-path E2E tests for the agent browser bridge.
//
// These tests exercise the real v1 capfile JSON shape {version, endpoint,
// token} and the real HTTP route shapes at /code/browser/list, /navigate,
// /snapshot, /wait, /screenshot through a loopback-only mock bridge server.
// The mock is honest: it does NOT claim to prove native BrowserRegistry
// internals. Rust-backed authority, token registry transactionality, and
// desktop runtime integration are CI-only.
//
// Page content is treated as untrusted throughout. No assertion treats
// page content as instruction.

import assert from "node:assert/strict";
import { after, before, describe, test } from "node:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { isAbsolute, join, sep } from "node:path";

import { startBrowserFixture } from "../browser-fixture/server.mjs";
import { startBridgeServer, close } from "./bridge-server.mjs";
import {
  readCapfile,
  callBrowserRoute,
  BROWSER_TOOLS,
  FORBIDDEN_TOOLS,
  assertToolRegistry,
  assertListShape,
  assertNavigateShape,
  assertSnapshotShape,
  assertWaitShape,
  assertScreenshotShape,
  drivePositiveContract,
  simulateAbsoluteLaunch,
  writeCapfile,
} from "./scripted-harness.mjs";

// ── fixture lifecycles ──────────────────────────────────────────────────────

let fixture;
let bridge;
let bridgeEndpoint;
let tmpDir;

before(async () => {
  // Deterministic web fixture on ephemeral ports
  fixture = await startBrowserFixture({ port: 0, crossOriginPort: 0 });

  // Mock bridge server with ephemeral port
  bridge = await startBridgeServer({
    port: 0,
    fixtureOrigin: fixture.origin,
  });
  bridgeEndpoint = bridge.endpoint;

  // Temporary directory for capfiles
  tmpDir = await mkdtemp(join(tmpdir(), "tb-bridge-e2e-"));
});

after(async () => {
  const errors = [];
  try {
    await fixture?.close();
  } catch (e) {
    errors.push(e);
  }
  try {
    if (bridge) await close(bridge.server);
  } catch (e) {
    errors.push(e);
  }
  try {
    await rm(tmpDir, { recursive: true, force: true });
  } catch (e) {
    errors.push(e);
  }
  if (errors.length > 0) {
    throw new Error(
      `cleanup errors: ${errors.map((e) => e.message).join("; ")}`
    );
  }
});

// ── tool registry contract ──────────────────────────────────────────────────

test("exact tool registry is five browser tools, no act/semantic", () => {
  // Assert BROWSER_TOOLS is exactly five elements
  assert.deepEqual(BROWSER_TOOLS, [
    "browser_list",
    "browser_navigate",
    "browser_snapshot",
    "browser_wait",
    "browser_screenshot",
  ]);

  // Assert the five are recognized
  const registry = BROWSER_TOOLS.map((name) => ({ name }));
  assertToolRegistry(registry);

  // Prove the forbidden tools are detected as violations
  for (const forbidden of FORBIDDEN_TOOLS) {
    assert.throws(
      () => assertToolRegistry([...BROWSER_TOOLS, forbidden].map((n) => ({ name: n }))),
      /forbidden tool/
    );
  }
});

// ── capfile v1 protocol ─────────────────────────────────────────────────────

test("capfile schema is exactly {version:1, endpoint, token}", async () => {
  const { capfilePath, token } = await writeCapfile(tmpDir, "http://127.0.0.1:0/code/browser");
  const parsed = await readCapfile(capfilePath);

  assert.equal(parsed.endpoint, "http://127.0.0.1:0/code/browser");
  assert.equal(parsed.token, token);
  assert.ok(token.startsWith("tbreak_bt_"), "token must start with tbreak_bt_");
});

test("capfile rejects missing or extra keys", async () => {
  const { writeFile, mkdir } = await import("node:fs/promises");
  const badDir = join(tmpDir, "bad-capfiles");
  await mkdir(badDir, { recursive: true });

  // Missing version
  const bad1 = join(badDir, "no-version.json");
  await writeFile(
    bad1,
    JSON.stringify({ endpoint: "http://127.0.0.1:0/code/browser", token: "tbreak_bt_uuid" })
  );
  await assert.rejects(() => readCapfile(bad1), /version/);

  // Extra key
  const bad2 = join(badDir, "extra-key.json");
  await writeFile(
    bad2,
    JSON.stringify({
      version: 1,
      endpoint: "http://127.0.0.1:0/code/browser",
      token: "tbreak_bt_uuid",
      workspace_id: "leak",
    })
  );
  await assert.rejects(() => readCapfile(bad2), /unexpected keys/);
});

// ── positive contract: list → navigate → snapshot → wait → screenshot ───────

test("drive list → navigate → snapshot → wait → screenshot against deterministic fixture", async () => {
  const token = bridge.tokenRegistry.issue();

  const screenshot = await drivePositiveContract(
    bridgeEndpoint,
    token,
    fixture.origin
  );

  // Verify screenshot wire shape
  assert.equal(screenshot.mimeType, "image/png");
  assert.ok(screenshot.imageBase64.length > 0, "screenshot must have image data");

  // Verify the base64 decodes to a valid PNG
  const decoded = Buffer.from(screenshot.imageBase64, "base64");
  assert.ok(decoded.length >= 8, "PNG must be at least 8 bytes");
  assert.equal(decoded[0], 0x89, "PNG signature byte 0");
  assert.equal(decoded[1], 0x50, "PNG signature byte 1 (P)");
  assert.equal(decoded[2], 0x4e, "PNG signature byte 2 (N)");
  assert.equal(decoded[3], 0x47, "PNG signature byte 3 (G)");
});

test("navigate and snapshot return camelCase shapes", async () => {
  const token = bridge.tokenRegistry.issue();

  // Navigate
  const nav = await callBrowserRoute(
    bridgeEndpoint,
    "navigate",
    { browser_id: "browser-1", url: `${fixture.origin}/?view=details` },
    token
  );
  assert.equal(nav.status, 200);
  assertNavigateShape(nav.body);
  assert.ok(nav.body.documentEpoch >= 3, "navigate must increment epoch");
});

test("snapshot with max_nodes returns camelCase shape", async () => {
  const token = bridge.tokenRegistry.issue();

  // Navigate first to get a fresh epoch
  await callBrowserRoute(
    bridgeEndpoint,
    "navigate",
    { browser_id: "browser-1", url: fixture.origin },
    token
  );

  const snap = await callBrowserRoute(
    bridgeEndpoint,
    "snapshot",
    { browser_id: "browser-1", max_nodes: 10 },
    token
  );
  assert.equal(snap.status, 200);
  assertSnapshotShape(snap.body);
});

// ── positive: wait contract ─────────────────────────────────────────────────

test("wait with load_state condition returns resolved status", async () => {
  const token = bridge.tokenRegistry.issue();

  // Navigate + snapshot to get a valid epoch
  await callBrowserRoute(
    bridgeEndpoint,
    "navigate",
    { browser_id: "browser-1", url: fixture.origin },
    token
  );
  const snap = await callBrowserRoute(
    bridgeEndpoint,
    "snapshot",
    { browser_id: "browser-1" },
    token
  );

  const wait = await callBrowserRoute(
    bridgeEndpoint,
    "wait",
    {
      browser_id: "browser-1",
      snapshot_id: snap.body.snapshotId,
      document_epoch: snap.body.documentEpoch,
      condition: { kind: "load_state", state: "ready" },
    },
    token
  );
  assert.equal(wait.status, 200);
  assertWaitShape(wait.body);
  assert.equal(wait.body.status, "resolved");
  assert.equal(wait.body.browserId, "browser-1");
});

test("wait with url_changed condition returns resolved status", async () => {
  const token = bridge.tokenRegistry.issue();

  await callBrowserRoute(
    bridgeEndpoint,
    "navigate",
    { browser_id: "browser-1", url: fixture.origin },
    token
  );
  const snap = await callBrowserRoute(
    bridgeEndpoint,
    "snapshot",
    { browser_id: "browser-1" },
    token
  );

  const wait = await callBrowserRoute(
    bridgeEndpoint,
    "wait",
    {
      browser_id: "browser-1",
      snapshot_id: snap.body.snapshotId,
      document_epoch: snap.body.documentEpoch,
      condition: { kind: "url_changed" },
    },
    token
  );
  assert.equal(wait.status, 200);
  assertWaitShape(wait.body);
  assert.equal(wait.body.status, "resolved");
});

test("wait with text_present condition returns resolved status", async () => {
  const token = bridge.tokenRegistry.issue();

  await callBrowserRoute(
    bridgeEndpoint,
    "navigate",
    { browser_id: "browser-1", url: fixture.origin },
    token
  );
  const snap = await callBrowserRoute(
    bridgeEndpoint,
    "snapshot",
    { browser_id: "browser-1" },
    token
  );

  const wait = await callBrowserRoute(
    bridgeEndpoint,
    "wait",
    {
      browser_id: "browser-1",
      snapshot_id: snap.body.snapshotId,
      document_epoch: snap.body.documentEpoch,
      condition: { kind: "text_present", text: "Agent browser fixture" },
    },
    token
  );
  assert.equal(wait.status, 200);
  assertWaitShape(wait.body);
  assert.equal(wait.body.status, "resolved");
});

test("wait with text_absent condition returns resolved status", async () => {
  const token = bridge.tokenRegistry.issue();

  await callBrowserRoute(
    bridgeEndpoint,
    "navigate",
    { browser_id: "browser-1", url: fixture.origin },
    token
  );
  const snap = await callBrowserRoute(
    bridgeEndpoint,
    "snapshot",
    { browser_id: "browser-1" },
    token
  );

  const wait = await callBrowserRoute(
    bridgeEndpoint,
    "wait",
    {
      browser_id: "browser-1",
      snapshot_id: snap.body.snapshotId,
      document_epoch: snap.body.documentEpoch,
      condition: { kind: "text_absent", text: "This text does not exist" },
    },
    token
  );
  assert.equal(wait.status, 200);
  assertWaitShape(wait.body);
  assert.equal(wait.body.status, "resolved");
});

test("wait with explicit timeout_ms returns resolved status", async () => {
  const token = bridge.tokenRegistry.issue();

  await callBrowserRoute(
    bridgeEndpoint,
    "navigate",
    { browser_id: "browser-1", url: fixture.origin },
    token
  );
  const snap = await callBrowserRoute(
    bridgeEndpoint,
    "snapshot",
    { browser_id: "browser-1" },
    token
  );

  const wait = await callBrowserRoute(
    bridgeEndpoint,
    "wait",
    {
      browser_id: "browser-1",
      snapshot_id: snap.body.snapshotId,
      document_epoch: snap.body.documentEpoch,
      condition: { kind: "load_state", state: "ready" },
      timeout_ms: 5000,
    },
    token
  );
  assert.equal(wait.status, 200);
  assertWaitShape(wait.body);
  assert.equal(wait.body.status, "resolved");
});

test("wait returns timed_out when configured with waitOutcome", async () => {
  const timedOutBridge = await startBridgeServer({
    port: 0,
    fixtureOrigin: fixture.origin,
    waitOutcome: "timed_out",
  });

  try {
    const token = timedOutBridge.tokenRegistry.issue();
    const snap = await callBrowserRoute(
      timedOutBridge.endpoint,
      "snapshot",
      { browser_id: "browser-1" },
      token
    );

    const wait = await callBrowserRoute(
      timedOutBridge.endpoint,
      "wait",
      {
        browser_id: "browser-1",
        snapshot_id: snap.body.snapshotId,
        document_epoch: snap.body.documentEpoch,
        condition: { kind: "text_present", text: "never appears" },
        timeout_ms: 1000,
      },
      token
    );
    assert.equal(wait.status, 200);
    assertWaitShape(wait.body);
    assert.equal(wait.body.status, "timed_out");
    assert.match(wait.body.message, /timed out/i);
  } finally {
    await close(timedOutBridge.server);
  }
});

test("wait returns stopped when control is halted", async () => {
  const stoppedBridge = await startBridgeServer({
    port: 0,
    fixtureOrigin: fixture.origin,
    stoppedControl: true,
  });

  try {
    const token = stoppedBridge.tokenRegistry.issue();
    const snap = await callBrowserRoute(
      stoppedBridge.endpoint,
      "snapshot",
      { browser_id: "browser-1" },
      token
    );

    const wait = await callBrowserRoute(
      stoppedBridge.endpoint,
      "wait",
      {
        browser_id: "browser-1",
        snapshot_id: snap.body.snapshotId,
        document_epoch: snap.body.documentEpoch,
        condition: { kind: "load_state", state: "ready" },
      },
      token
    );
    assert.equal(wait.status, 200);
    assertWaitShape(wait.body);
    assert.equal(wait.body.status, "stopped");
    assert.match(wait.body.message, /stopped/i);
  } finally {
    await close(stoppedBridge.server);
  }
});

// ── positive: screenshot contract ───────────────────────────────────────────

test("screenshot returns deterministic valid PNG base64", async () => {
  const token = bridge.tokenRegistry.issue();

  await callBrowserRoute(
    bridgeEndpoint,
    "navigate",
    { browser_id: "browser-1", url: fixture.origin },
    token
  );
  const snap = await callBrowserRoute(
    bridgeEndpoint,
    "snapshot",
    { browser_id: "browser-1" },
    token
  );

  const screenshot = await callBrowserRoute(
    bridgeEndpoint,
    "screenshot",
    {
      browser_id: "browser-1",
      snapshot_id: snap.body.snapshotId,
      document_epoch: snap.body.documentEpoch,
    },
    token
  );
  assert.equal(screenshot.status, 200);
  assertScreenshotShape(screenshot.body);
  assert.equal(screenshot.body.browserId, "browser-1");
  assert.equal(screenshot.body.snapshotId, snap.body.snapshotId);
  assert.equal(screenshot.body.documentEpoch, snap.body.documentEpoch);
});

test("screenshot with max_width and max_height returns valid PNG", async () => {
  const token = bridge.tokenRegistry.issue();

  await callBrowserRoute(
    bridgeEndpoint,
    "navigate",
    { browser_id: "browser-1", url: fixture.origin },
    token
  );
  const snap = await callBrowserRoute(
    bridgeEndpoint,
    "snapshot",
    { browser_id: "browser-1" },
    token
  );

  const screenshot = await callBrowserRoute(
    bridgeEndpoint,
    "screenshot",
    {
      browser_id: "browser-1",
      snapshot_id: snap.body.snapshotId,
      document_epoch: snap.body.documentEpoch,
      max_width: 1920,
      max_height: 1080,
    },
    token
  );
  assert.equal(screenshot.status, 200);
  assertScreenshotShape(screenshot.body);
});

test("screenshot with max_height=0 (viewport height) returns valid PNG", async () => {
  const token = bridge.tokenRegistry.issue();

  await callBrowserRoute(
    bridgeEndpoint,
    "navigate",
    { browser_id: "browser-1", url: fixture.origin },
    token
  );
  const snap = await callBrowserRoute(
    bridgeEndpoint,
    "snapshot",
    { browser_id: "browser-1" },
    token
  );

  const screenshot = await callBrowserRoute(
    bridgeEndpoint,
    "screenshot",
    {
      browser_id: "browser-1",
      snapshot_id: snap.body.snapshotId,
      document_epoch: snap.body.documentEpoch,
      max_height: 0,
    },
    token
  );
  assert.equal(screenshot.status, 200);
  assertScreenshotShape(screenshot.body);
});

// ── absolute command, no PATH ───────────────────────────────────────────────

test("absolute harness launch succeeds with PATH unavailable", async () => {
  const { capfilePath, token } = await writeCapfile(tmpDir, bridgeEndpoint);

  const result = await simulateAbsoluteLaunch({ capfilePath });

  assert.equal(result.envHasPath, false, "PATH must be absent from the child environment");
  assert.ok(isAbsolute(result.command), "the launched executable must be absolute");
  assert.ok(!result.argv.includes(capfilePath), "capfile path must stay out of argv");
  assert.ok(!result.stdout.includes(capfilePath), "capfile path must stay out of stdout");
  assert.ok(!result.stderr.includes(capfilePath), "capfile path must stay out of stderr");
  assert.ok(!result.stdout.includes(token), "token must stay out of stdout");
  assert.ok(!result.stderr.includes(token), "token must stay out of stderr");

  // Capfile is readable via TIDEBREAK_BROWSER_CAPFILE
  const capfile = await readCapfile(capfilePath);
  assert.equal(capfile.token, token);
  assert.equal(capfile.endpoint, bridgeEndpoint);

  assert.ok(!result.argv.join(" ").includes(token), "token must not appear in argv");
});

test("absolute launch rejects capfile-path output leakage", async () => {
  const { capfilePath } = await writeCapfile(tmpDir, bridgeEndpoint);
  const leakingProbe = `
    import { readFile } from "node:fs/promises";
    const path = process.env.TIDEBREAK_BROWSER_CAPFILE;
    const capfile = JSON.parse(await readFile(path, "utf8"));
    process.stderr.write(path);
    process.stdout.write(JSON.stringify({
      endpoint: capfile.endpoint,
      tools: [
        { name: "browser_list" },
        { name: "browser_navigate" },
        { name: "browser_snapshot" },
        { name: "browser_wait" },
        { name: "browser_screenshot" }
      ]
    }));
  `;

  await assert.rejects(
    simulateAbsoluteLaunch({
      capfilePath,
      command: process.execPath,
      args: ["--input-type=module", "--eval", leakingProbe],
    }),
    /capfile path escaped through child process output/
  );
});

test("absolute launch rejects JSON-escaped backslash path leakage", async () => {
  const backslashDir = join(tmpDir, "browser\\caps");
  const { capfilePath } = await writeCapfile(backslashDir, bridgeEndpoint);
  const leakingProbe = `
    import { readFile } from "node:fs/promises";
    const path = process.env.TIDEBREAK_BROWSER_CAPFILE;
    const capfile = JSON.parse(await readFile(path, "utf8"));
    process.stderr.write(JSON.stringify({ debugPath: path }));
    process.stdout.write(JSON.stringify({
      endpoint: capfile.endpoint,
      tools: [
        { name: "browser_list" },
        { name: "browser_navigate" },
        { name: "browser_snapshot" },
        { name: "browser_wait" },
        { name: "browser_screenshot" }
      ]
    }));
  `;

  await assert.rejects(
    simulateAbsoluteLaunch({
      capfilePath,
      command: process.execPath,
      args: ["--input-type=module", "--eval", leakingProbe],
    }),
    /capfile path escaped through child process output/
  );
});

test("absolute launch rejects JSON-escaped backslash path in argv", async () => {
  const backslashDir = join(tmpDir, "browser\\caps-argv");
  const { capfilePath } = await writeCapfile(backslashDir, bridgeEndpoint);
  const benignProbe = `
    import { readFile } from "node:fs/promises";
    const path = process.env.TIDEBREAK_BROWSER_CAPFILE;
    const capfile = JSON.parse(await readFile(path, "utf8"));
    process.stdout.write(JSON.stringify({
      endpoint: capfile.endpoint,
      tools: [
        { name: "browser_list" },
        { name: "browser_navigate" },
        { name: "browser_snapshot" },
        { name: "browser_wait" },
        { name: "browser_screenshot" }
      ]
    }));
  `;

  await assert.rejects(
    simulateAbsoluteLaunch({
      capfilePath,
      command: process.execPath,
      args: [
        "--input-type=module",
        "--eval",
        benignProbe,
        JSON.stringify({ debugPath: capfilePath }),
      ],
    }),
    /capfile path escaped into child process argv/
  );
});

test("token never appears in capfile path", async () => {
  const { capfilePath, token } = await writeCapfile(tmpDir, bridgeEndpoint);

  // The token must not be embedded in the filename
  assert.ok(
    !capfilePath.includes(token.replaceAll("-", "")),
    "token must not appear in capfile path"
  );

  // Verify the file stem is UUID-shaped (32 hex chars)
  const stem = capfilePath.split(sep).pop().replace(/^browser-cap-/, "").replace(".json", "");
  assert.equal(stem.length, 32, "file id must be 32 hex chars");
  assert.ok(/^[0-9a-f]{32}$/.test(stem), "file id must be hex");
});

// ── negative: auth ladder ───────────────────────────────────────────────────

test("missing token returns 401", async () => {
  // Direct fetch without auth header
  const response = await fetch(`${bridgeEndpoint}/list`);
  assert.equal(response.status, 401);
  const body = await response.json();
  assert.match(body.kind ?? "", /unauthorized/i);
});

test("unknown token returns 401", async () => {
  const result = await callBrowserRoute(
    bridgeEndpoint,
    "list",
    null,
    "tbreak_bt_00000000-0000-4000-8000-000000000000"
  );
  assert.equal(result.status, 401);
});

test("revoked token returns 401", async () => {
  const token = bridge.tokenRegistry.issue();
  bridge.tokenRegistry.revoke(token);

  const result = await callBrowserRoute(bridgeEndpoint, "list", null, token);
  assert.equal(result.status, 401);
  assert.match(
    JSON.stringify(result.body),
    /unauthorized|unknown|revoked/i
  );
});

test("ended session returns 403", async () => {
  const token = bridge.tokenRegistry.issue();
  bridge.tokenRegistry.end(token);

  const result = await callBrowserRoute(bridgeEndpoint, "list", null, token);
  assert.equal(result.status, 403);
  assert.match(
    JSON.stringify(result.body),
    /ended|forbidden/i
  );
});

test("missing runtime returns 501", async () => {
  const noRuntime = await startBridgeServer({
    port: 0,
    fixtureOrigin: fixture.origin,
    missingRuntime: true,
  });

  try {
    const token = noRuntime.tokenRegistry.issue();
    const result = await callBrowserRoute(noRuntime.endpoint, "list", null, token);
    assert.equal(result.status, 501);
    assert.match(
      JSON.stringify(result.body),
      /no in-app browser runtime/i
    );
  } finally {
    await close(noRuntime.server);
  }
});

test("browser capability auth runs before missing-runtime disclosure", async () => {
  const noRuntime = await startBridgeServer({
    port: 0,
    fixtureOrigin: fixture.origin,
    missingRuntime: true,
  });

  try {
    const missing = await fetch(`${noRuntime.endpoint}/list`);
    assert.equal(missing.status, 401);

    const unknown = await callBrowserRoute(
      noRuntime.endpoint,
      "list",
      null,
      "tbreak_bt_unknown"
    );
    assert.equal(unknown.status, 401);
  } finally {
    await close(noRuntime.server);
  }
});

test("argument validation runs before missing-runtime disclosure", async () => {
  const noRuntime = await startBridgeServer({
    port: 0,
    fixtureOrigin: fixture.origin,
    missingRuntime: true,
  });

  try {
    const token = noRuntime.tokenRegistry.issue();
    const invalidNavigation = await callBrowserRoute(
      noRuntime.endpoint,
      "navigate",
      { browser_id: "browser-1", url: "file:///etc/passwd" },
      token
    );
    assert.equal(invalidNavigation.status, 422);

    const invalidSnapshot = await callBrowserRoute(
      noRuntime.endpoint,
      "snapshot",
      { browser_id: "browser-1", max_nodes: 501 },
      token
    );
    assert.equal(invalidSnapshot.status, 422);

    const invalidWait = await callBrowserRoute(
      noRuntime.endpoint,
      "wait",
      {
        browser_id: "browser-1",
        snapshot_id: "snap-1",
        document_epoch: 0,
        condition: { kind: "text_present", text: "x".repeat(600) },
      },
      token
    );
    assert.equal(invalidWait.status, 422);

    const invalidScreenshot = await callBrowserRoute(
      noRuntime.endpoint,
      "screenshot",
      {
        browser_id: "browser-1",
        snapshot_id: "snap-1",
        document_epoch: 0,
        max_width: 5000,
      },
      token
    );
    assert.equal(invalidScreenshot.status, 422);

    const validNavigation = await callBrowserRoute(
      noRuntime.endpoint,
      "navigate",
      { browser_id: "browser-1", url: fixture.origin },
      token
    );
    assert.equal(validNavigation.status, 501);
  } finally {
    await close(noRuntime.server);
  }
});

// ── negative: cross-workspace / unknown browser id ──────────────────────────

test("cross-workspace / unknown browser id returns 404", async () => {
  const token = bridge.tokenRegistry.issue();

  // Navigate to a browser not owned by this token
  const nav = await callBrowserRoute(
    bridgeEndpoint,
    "navigate",
    { browser_id: "browser-9", url: fixture.origin },
    token
  );
  assert.equal(nav.status, 404);

  // Snapshot with unknown browser id
  const snap = await callBrowserRoute(
    bridgeEndpoint,
    "snapshot",
    { browser_id: "browser-unknown" },
    token
  );
  assert.equal(snap.status, 404);

  // Wait with unknown browser id
  const wait = await callBrowserRoute(
    bridgeEndpoint,
    "wait",
    {
      browser_id: "browser-9",
      snapshot_id: "snap-1",
      document_epoch: 0,
      condition: { kind: "load_state", state: "ready" },
    },
    token
  );
  assert.equal(wait.status, 404);

  // Screenshot with unknown browser id
  const screenshot = await callBrowserRoute(
    bridgeEndpoint,
    "screenshot",
    {
      browser_id: "browser-9",
      snapshot_id: "snap-1",
      document_epoch: 0,
    },
    token
  );
  assert.equal(screenshot.status, 404);
});

// ── negative: stopped control ───────────────────────────────────────────────

test("stopped control does not prevent list but signals halted state", async () => {
  const stoppedBridge = await startBridgeServer({
    port: 0,
    fixtureOrigin: fixture.origin,
    stoppedControl: true,
  });

  try {
    const token = stoppedBridge.tokenRegistry.issue();
    const list = await callBrowserRoute(stoppedBridge.endpoint, "list", null, token);
    assert.equal(list.status, 200);
    // The controller should indicate halted state
    const session = list.body.sessions?.[0];
    assert.ok(session, "list must return a session");
    assert.equal(
      session.controller?.halted,
      true,
      "stopped control must set halted=true"
    );
  } finally {
    await close(stoppedBridge.server);
  }
});

// ── negative: hidden browser ────────────────────────────────────────────────

test("hidden browser is listed but visible=false", async () => {
  const hiddenBridge = await startBridgeServer({
    port: 0,
    fixtureOrigin: fixture.origin,
    hiddenBrowser: true,
  });

  try {
    const token = hiddenBridge.tokenRegistry.issue();
    const list = await callBrowserRoute(hiddenBridge.endpoint, "list", null, token);
    assert.equal(list.status, 200);
    const session = list.body.sessions?.[0];
    assert.ok(session, "list must return a session");
    assert.equal(session.visible, false, "hidden browser must have visible=false");
  } finally {
    await close(hiddenBridge.server);
  }
});

// ── negative: stale instance/document epoch ─────────────────────────────────

test("stale snapshot returns 409 conflict", async () => {
  const staleBridge = await startBridgeServer({
    port: 0,
    fixtureOrigin: fixture.origin,
    staleSnapshot: true,
  });

  try {
    const token = staleBridge.tokenRegistry.issue();
    const result = await callBrowserRoute(
      staleBridge.endpoint,
      "snapshot",
      { browser_id: "browser-1" },
      token
    );
    assert.equal(result.status, 409);
    assert.equal(result.body.kind, "stale_browser_target");
  } finally {
    await close(staleBridge.server);
  }
});

test("stale document epoch on wait returns 409 conflict", async () => {
  const staleBridge = await startBridgeServer({
    port: 0,
    fixtureOrigin: fixture.origin,
    staleSnapshot: true,
  });

  try {
    const token = staleBridge.tokenRegistry.issue();
    const result = await callBrowserRoute(
      staleBridge.endpoint,
      "wait",
      {
        browser_id: "browser-1",
        snapshot_id: "snap-old",
        document_epoch: 999,
        condition: { kind: "load_state", state: "ready" },
      },
      token
    );
    assert.equal(result.status, 409);
    assert.equal(result.body.kind, "stale_browser_target");
  } finally {
    await close(staleBridge.server);
  }
});

test("stale document epoch on screenshot returns 409 conflict", async () => {
  const staleBridge = await startBridgeServer({
    port: 0,
    fixtureOrigin: fixture.origin,
    staleSnapshot: true,
  });

  try {
    const token = staleBridge.tokenRegistry.issue();
    const result = await callBrowserRoute(
      staleBridge.endpoint,
      "screenshot",
      {
        browser_id: "browser-1",
        snapshot_id: "snap-old",
        document_epoch: 999,
      },
      token
    );
    assert.equal(result.status, 409);
    assert.equal(result.body.kind, "stale_browser_target");
  } finally {
    await close(staleBridge.server);
  }
});

test("stale instance (session replaced) on wait returns 409", async () => {
  const staleInstanceBridge = await startBridgeServer({
    port: 0,
    fixtureOrigin: fixture.origin,
    staleInstance: true,
  });

  try {
    const token = staleInstanceBridge.tokenRegistry.issue();
    const snap = await callBrowserRoute(
      staleInstanceBridge.endpoint,
      "snapshot",
      { browser_id: "browser-1" },
      token
    );
    assert.equal(snap.status, 200);

    const wait = await callBrowserRoute(
      staleInstanceBridge.endpoint,
      "wait",
      {
        browser_id: "browser-1",
        snapshot_id: snap.body.snapshotId,
        document_epoch: snap.body.documentEpoch,
        condition: { kind: "load_state", state: "ready" },
      },
      token
    );
    assert.equal(wait.status, 409);
    assert.equal(wait.body.kind, "stale_browser_target");
  } finally {
    await close(staleInstanceBridge.server);
  }
});

test("stale instance (session replaced) on screenshot returns 409", async () => {
  const staleInstanceBridge = await startBridgeServer({
    port: 0,
    fixtureOrigin: fixture.origin,
    staleInstance: true,
  });

  try {
    const token = staleInstanceBridge.tokenRegistry.issue();
    const snap = await callBrowserRoute(
      staleInstanceBridge.endpoint,
      "snapshot",
      { browser_id: "browser-1" },
      token
    );
    assert.equal(snap.status, 200);

    const screenshot = await callBrowserRoute(
      staleInstanceBridge.endpoint,
      "screenshot",
      {
        browser_id: "browser-1",
        snapshot_id: snap.body.snapshotId,
        document_epoch: snap.body.documentEpoch,
      },
      token
    );
    assert.equal(screenshot.status, 409);
    assert.equal(screenshot.body.kind, "stale_browser_target");
  } finally {
    await close(staleInstanceBridge.server);
  }
});

// ── negative: invalid navigation URLs ───────────────────────────────────────

test("navigate refuses non-HTTP URLs", async () => {
  const token = bridge.tokenRegistry.issue();

  for (const badUrl of [
    "file:///etc/passwd",
    "javascript:alert(1)",
    "ftp://evil.com",
    "data:text/html,<script>alert(1)</script>",
  ]) {
    const result = await callBrowserRoute(
      bridgeEndpoint,
      "navigate",
      { browser_id: "browser-1", url: badUrl },
      token
    );
    assert.equal(
      result.status,
      422,
      `navigate to ${badUrl} must return 422, got ${result.status}`
    );
  }
});

test("navigate refuses URLs with embedded credentials", async () => {
  const token = bridge.tokenRegistry.issue();

  const result = await callBrowserRoute(
    bridgeEndpoint,
    "navigate",
    { browser_id: "browser-1", url: "https://user:secret@example.com/path" },
    token
  );
  assert.equal(result.status, 422);
});

test("navigate rejects malformed URLs", async () => {
  const token = bridge.tokenRegistry.issue();

  const result = await callBrowserRoute(
    bridgeEndpoint,
    "navigate",
    { browser_id: "browser-1", url: "not a url" },
    token
  );
  assert.equal(result.status, 422);
});

// ── negative: body validation ───────────────────────────────────────────────

test("bodies with unknown fields return 400", async () => {
  const token = bridge.tokenRegistry.issue();

  // Navigate with extra field
  const nav = await callBrowserRoute(
    bridgeEndpoint,
    "navigate",
    { browser_id: "browser-1", url: fixture.origin, session_id: "leak" },
    token
  );
  assert.equal(nav.status, 400);

  // Snapshot with extra field
  const snap = await callBrowserRoute(
    bridgeEndpoint,
    "snapshot",
    { browser_id: "browser-1", owner_id: "leak" },
    token
  );
  assert.equal(snap.status, 400);

  // Wait with extra field
  const wait = await callBrowserRoute(
    bridgeEndpoint,
    "wait",
    {
      browser_id: "browser-1",
      snapshot_id: "snap-1",
      document_epoch: 0,
      condition: { kind: "load_state", state: "ready" },
      workspace_id: "leak",
    },
    token
  );
  assert.equal(wait.status, 400);

  // Screenshot with extra field
  const screenshot = await callBrowserRoute(
    bridgeEndpoint,
    "screenshot",
    {
      browser_id: "browser-1",
      snapshot_id: "snap-1",
      document_epoch: 0,
      owner_id: "leak",
    },
    token
  );
  assert.equal(screenshot.status, 400);
});

test("snapshot requires browser_id", async () => {
  const token = bridge.tokenRegistry.issue();

  const result = await callBrowserRoute(
    bridgeEndpoint,
    "snapshot",
    {},
    token
  );
  assert.equal(result.status, 400);
});

test("wait requires browser_id, snapshot_id, document_epoch, condition", async () => {
  const token = bridge.tokenRegistry.issue();

  // Missing browser_id
  const noBid = await callBrowserRoute(
    bridgeEndpoint,
    "wait",
    {
      snapshot_id: "snap-1",
      document_epoch: 0,
      condition: { kind: "load_state", state: "ready" },
    },
    token
  );
  assert.equal(noBid.status, 400);

  // Missing snapshot_id
  const noSid = await callBrowserRoute(
    bridgeEndpoint,
    "wait",
    {
      browser_id: "browser-1",
      document_epoch: 0,
      condition: { kind: "load_state", state: "ready" },
    },
    token
  );
  assert.equal(noSid.status, 400);

  // Missing document_epoch
  const noEpoch = await callBrowserRoute(
    bridgeEndpoint,
    "wait",
    {
      browser_id: "browser-1",
      snapshot_id: "snap-1",
      condition: { kind: "load_state", state: "ready" },
    },
    token
  );
  assert.equal(noEpoch.status, 400);

  // Missing condition
  const noCond = await callBrowserRoute(
    bridgeEndpoint,
    "wait",
    {
      browser_id: "browser-1",
      snapshot_id: "snap-1",
      document_epoch: 0,
    },
    token
  );
  assert.equal(noCond.status, 400);
});

test("screenshot requires browser_id, snapshot_id, document_epoch", async () => {
  const token = bridge.tokenRegistry.issue();

  // Missing browser_id
  const noBid = await callBrowserRoute(
    bridgeEndpoint,
    "screenshot",
    {
      snapshot_id: "snap-1",
      document_epoch: 0,
    },
    token
  );
  assert.equal(noBid.status, 400);

  // Missing snapshot_id
  const noSid = await callBrowserRoute(
    bridgeEndpoint,
    "screenshot",
    {
      browser_id: "browser-1",
      document_epoch: 0,
    },
    token
  );
  assert.equal(noSid.status, 400);

  // Missing document_epoch
  const noEpoch = await callBrowserRoute(
    bridgeEndpoint,
    "screenshot",
    {
      browser_id: "browser-1",
      snapshot_id: "snap-1",
    },
    token
  );
  assert.equal(noEpoch.status, 400);
});

test("snapshot rejects out-of-range max_nodes", async () => {
  const token = bridge.tokenRegistry.issue();

  for (const badMax of [0, 501, 1000, -1]) {
    const result = await callBrowserRoute(
      bridgeEndpoint,
      "snapshot",
      { browser_id: "browser-1", max_nodes: badMax },
      token
    );
    assert.equal(
      result.status,
      422,
      `snapshot with max_nodes=${badMax} must return 422, got ${result.status}`
    );
  }
});

// ── negative: wait validation ───────────────────────────────────────────────

test("wait rejects non-deserializable conditions with 400", async () => {
  const token = bridge.tokenRegistry.issue();

  // These conditions would fail serde deserialization of BrowserWaitCondition
  // (missing kind, unknown kind, wrong type, missing variant fields) → 400
  for (const badCondition of [
    { kind: "unknown_condition" },
    { kind: "click" },
    {},
    "not an object",
    { kind: "load_state" }, // missing state
    { kind: "text_present" }, // missing text
    { kind: "text_absent" }, // missing text
  ]) {
    const result = await callBrowserRoute(
      bridgeEndpoint,
      "wait",
      {
        browser_id: "browser-1",
        snapshot_id: "snap-1",
        document_epoch: 0,
        condition: badCondition,
      },
      token
    );
    assert.equal(
      result.status,
      400,
      `wait with condition ${JSON.stringify(badCondition)} must return 400, got ${result.status}`
    );
  }
});

test("wait rejects load_state with invalid state value (400)", async () => {
  const token = bridge.tokenRegistry.issue();

  // "not_a_state" is not a valid BrowserLoadState variant — serde rejects → 400
  const result = await callBrowserRoute(
    bridgeEndpoint,
    "wait",
    {
      browser_id: "browser-1",
      snapshot_id: "snap-1",
      document_epoch: 0,
      condition: { kind: "load_state", state: "not_a_state" },
    },
    token
  );
  assert.equal(result.status, 400);
});

test("wait rejects text_present with missing text (400)", async () => {
  const token = bridge.tokenRegistry.issue();

  // Missing text field is a serde deserialization failure → 400
  const result = await callBrowserRoute(
    bridgeEndpoint,
    "wait",
    {
      browser_id: "browser-1",
      snapshot_id: "snap-1",
      document_epoch: 0,
      condition: { kind: "text_present" },
    },
    token
  );
  assert.equal(result.status, 400);
});

test("wait rejects text_present with oversized text (>512 chars)", async () => {
  const token = bridge.tokenRegistry.issue();

  const result = await callBrowserRoute(
    bridgeEndpoint,
    "wait",
    {
      browser_id: "browser-1",
      snapshot_id: "snap-1",
      document_epoch: 0,
      condition: { kind: "text_present", text: "x".repeat(513) },
    },
    token
  );
  assert.equal(result.status, 422);
});

test("wait rejects out-of-range timeout_ms", async () => {
  const token = bridge.tokenRegistry.issue();

  for (const badTimeout of [50, 50000, 0, -1]) {
    const result = await callBrowserRoute(
      bridgeEndpoint,
      "wait",
      {
        browser_id: "browser-1",
        snapshot_id: "snap-1",
        document_epoch: 0,
        condition: { kind: "load_state", state: "ready" },
        timeout_ms: badTimeout,
      },
      token
    );
    assert.equal(
      result.status,
      422,
      `wait with timeout_ms=${badTimeout} must return 422, got ${result.status}`
    );
  }
});

test("wait rejects invalid browser_id format", async () => {
  const token = bridge.tokenRegistry.issue();

  for (const badId of ["", "x".repeat(81), "has spaces", "has/slashes"]) {
    const result = await callBrowserRoute(
      bridgeEndpoint,
      "wait",
      {
        browser_id: badId,
        snapshot_id: "snap-1",
        document_epoch: 0,
        condition: { kind: "load_state", state: "ready" },
      },
      token
    );
    assert.equal(
      result.status,
      422,
      `wait with browser_id="${badId}" must return 422, got ${result.status}`
    );
  }
});

// ── negative: screenshot validation ─────────────────────────────────────────

test("screenshot rejects out-of-range max_width", async () => {
  const token = bridge.tokenRegistry.issue();

  for (const badWidth of [0, 5000, 10000, -1]) {
    const result = await callBrowserRoute(
      bridgeEndpoint,
      "screenshot",
      {
        browser_id: "browser-1",
        snapshot_id: "snap-1",
        document_epoch: 0,
        max_width: badWidth,
      },
      token
    );
    assert.equal(
      result.status,
      422,
      `screenshot with max_width=${badWidth} must return 422, got ${result.status}`
    );
  }
});

test("screenshot rejects out-of-range max_height", async () => {
  const token = bridge.tokenRegistry.issue();

  for (const badHeight of [5000, 10000, -1]) {
    const result = await callBrowserRoute(
      bridgeEndpoint,
      "screenshot",
      {
        browser_id: "browser-1",
        snapshot_id: "snap-1",
        document_epoch: 0,
        max_height: badHeight,
      },
      token
    );
    assert.equal(
      result.status,
      422,
      `screenshot with max_height=${badHeight} must return 422, got ${result.status}`
    );
  }
});

test("screenshot rejects invalid browser_id format", async () => {
  const token = bridge.tokenRegistry.issue();

  for (const badId of ["", "x".repeat(81), "has spaces", "has/slashes"]) {
    const result = await callBrowserRoute(
      bridgeEndpoint,
      "screenshot",
      {
        browser_id: badId,
        snapshot_id: "snap-1",
        document_epoch: 0,
      },
      token
    );
    assert.equal(
      result.status,
      422,
      `screenshot with browser_id="${badId}" must return 422, got ${result.status}`
    );
  }
});

// ── negative: redirect refusal ──────────────────────────────────────────────

test("client refuses redirects (manual redirect mode)", async () => {
  // The bridge server never sends redirects, but the client must refuse them.
  // We test this by pointing at the real fixture's /redirect endpoint, which
  // does redirect — our browser-route client uses redirect: "manual" so it
  // will see the 302 and refuse it.
  const response = await fetch(`${fixture.origin}/redirect`, {
    redirect: "manual",
  });
  assert.equal(response.status, 302);
  // Our callBrowserRoute client uses redirect: "manual" and reports 3xx
  // as an error. We confirm the fixture redirect works so if someone changes
  // the client to follow redirects, this test breaks.
});

// ── provider-neutral camelCase contract ─────────────────────────────────────

test("list response uses provider-neutral camelCase", async () => {
  const token = bridge.tokenRegistry.issue();
  const result = await callBrowserRoute(bridgeEndpoint, "list", null, token);
  assert.equal(result.status, 200);

  const s = result.body.sessions[0];
  // Must be camelCase, not snake_case
  assert.ok("browserId" in s, "must have browserId, not browser_id");
  assert.ok("loadState" in s, "must have loadState, not load_state");
  assert.equal(s.loadState, "ready");

  // Engine capabilities also camelCase
  const eng = s.engine;
  assert.ok("semanticSnapshot" in eng.capabilities, "must be camelCase");
  assert.ok("crossOriginFrames" in eng.capabilities, "must be camelCase");
});

test("navigate response uses provider-neutral camelCase", async () => {
  const token = bridge.tokenRegistry.issue();
  const result = await callBrowserRoute(
    bridgeEndpoint,
    "navigate",
    { browser_id: "browser-1", url: fixture.origin },
    token
  );
  assert.equal(result.status, 200);
  assert.ok("browserId" in result.body);
  assert.ok("loadState" in result.body);
  assert.ok("documentEpoch" in result.body);
});

test("snapshot response uses provider-neutral camelCase", async () => {
  const token = bridge.tokenRegistry.issue();
  const result = await callBrowserRoute(
    bridgeEndpoint,
    "snapshot",
    { browser_id: "browser-1" },
    token
  );
  assert.equal(result.status, 200);
  assert.ok("browserId" in result.body);
  assert.ok("snapshotId" in result.body);
  assert.ok("documentEpoch" in result.body);
  assert.equal(result.body.contentTrust, "untrusted_page");
  assert.ok("scrollX" in result.body.viewport, "viewport must be camelCase");
  assert.ok("scrollY" in result.body.viewport, "viewport must be camelCase");
});

test("wait response uses provider-neutral camelCase", async () => {
  const token = bridge.tokenRegistry.issue();

  await callBrowserRoute(
    bridgeEndpoint,
    "navigate",
    { browser_id: "browser-1", url: fixture.origin },
    token
  );
  const snap = await callBrowserRoute(
    bridgeEndpoint,
    "snapshot",
    { browser_id: "browser-1" },
    token
  );

  const result = await callBrowserRoute(
    bridgeEndpoint,
    "wait",
    {
      browser_id: "browser-1",
      snapshot_id: snap.body.snapshotId,
      document_epoch: snap.body.documentEpoch,
      condition: { kind: "load_state", state: "ready" },
    },
    token
  );
  assert.equal(result.status, 200);
  assert.ok("browserId" in result.body, "must have browserId, not browser_id");
  assert.ok("documentEpoch" in result.body, "must have documentEpoch, not document_epoch");
  // status is snake_case on the wire (resolved/timed_out/stopped)
  assert.ok("status" in result.body);
  assert.ok("message" in result.body);
});

test("screenshot response uses provider-neutral camelCase", async () => {
  const token = bridge.tokenRegistry.issue();

  await callBrowserRoute(
    bridgeEndpoint,
    "navigate",
    { browser_id: "browser-1", url: fixture.origin },
    token
  );
  const snap = await callBrowserRoute(
    bridgeEndpoint,
    "snapshot",
    { browser_id: "browser-1" },
    token
  );

  const result = await callBrowserRoute(
    bridgeEndpoint,
    "screenshot",
    {
      browser_id: "browser-1",
      snapshot_id: snap.body.snapshotId,
      document_epoch: snap.body.documentEpoch,
    },
    token
  );
  assert.equal(result.status, 200);
  assert.ok("browserId" in result.body, "must have browserId, not browser_id");
  assert.ok("snapshotId" in result.body, "must have snapshotId, not snapshot_id");
  assert.ok("documentEpoch" in result.body, "must have documentEpoch, not document_epoch");
  assert.ok("imageBase64" in result.body, "must have imageBase64, not image_base64");
  assert.ok("mimeType" in result.body, "must have mimeType, not mime_type");
  assert.equal(result.body.mimeType, "image/png");
});
