import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { isAbsolute, resolve } from "node:path";
import { promisify } from "node:util";
import { pathToFileURL } from "node:url";

const exec = promisify(execFile);

export function fixtureOrigin(value) {
  const url = new URL(value);
  assert.equal(url.protocol, "http:", "fixture must use loopback HTTP");
  assert.ok(
    ["127.0.0.1", "[::1]", "localhost"].includes(url.hostname),
    "fixture must be loopback",
  );
  assert.ok(url.port, "fixture must include its port");
  assert.ok(
    !url.username && !url.password && !url.search && !url.hash && url.pathname === "/",
    "pass only the fixture origin",
  );
  return url.origin;
}

function snapshotArgs(snapshot) {
  return [
    "--browser-id",
    snapshot.browserId,
    "--snapshot-id",
    snapshot.snapshotId,
    "--document-epoch",
    String(snapshot.documentEpoch),
  ];
}

function target(snapshot, name, action) {
  const nodes = snapshot.nodes.filter(
    (node) => node.name === name && node.kind === "interactive",
  );
  assert.equal(nodes.length, 1, "fixture must expose exactly one " + name + " target");
  assert.ok(
    nodes[0].ref && nodes[0].actions.includes(action),
    name + " must support " + action,
  );
  return nodes[0].ref;
}

function assertSnapshot(snapshot, browserId, origin) {
  assert.equal(snapshot.browserId, browserId);
  assert.equal(new URL(snapshot.url).origin, origin, "browser left the fixture origin");
  assert.equal(snapshot.contentTrust, "untrusted_page");
  assert.ok(snapshot.snapshotId && Number.isInteger(snapshot.documentEpoch));
  assert.ok(Array.isArray(snapshot.nodes));
  assert.equal(
    snapshot.truncated,
    false,
    "fixture snapshot must include every acceptance target",
  );
}

/** Drive the shipped CLI; injection is only for tests of the runner itself. */
export async function runNativeSmoke({
  call,
  origin,
  browserId,
  itemLabel = "Browser acceptance " + randomUUID(),
  readItems,
  pause = (ms) => new Promise((done) => setTimeout(done, ms)),
}) {
  origin = fixtureOrigin(origin);
  const list = await call(["list"]);
  const candidates = list.sessions.filter(
    (session) =>
      session.visible &&
      session.url &&
      new URL(session.url).origin === origin &&
      (!browserId || session.browserId === browserId),
  );
  assert.equal(
    candidates.length,
    1,
    "open and share exactly one visible fixture tab in this Tidebreak session",
  );
  const session = candidates[0];
  browserId = session.browserId;
  assert.equal(
    session.engine.name,
    "wk_web_view",
    "native acceptance requires WKWebView",
  );
  assert.equal(
    session.engine.capabilities.semanticActions,
    true,
    "native semantic actions must be advertised",
  );
  const before = await readItems();
  assert.ok(
    !before.some((item) => item.label === itemLabel),
    "acceptance label must be unique",
  );
  await call(["navigate", "--browser-id", browserId, "--url", origin + "/"]);
  let ready = false;
  for (let attempt = 0; attempt < 25; attempt += 1) {
    const state = (await call(["list"])).sessions.find(
      (entry) => entry.browserId === browserId,
    );
    assert.ok(state?.visible, "keep the fixture tab visible during acceptance");
    assert.notEqual(state.loadState, "failed", "fixture navigation failed");
    if (state.loadState === "ready" && state.url === origin + "/") {
      ready = true;
      break;
    }
    await pause(200);
  }
  assert.ok(ready, "fixture navigation did not become ready after 25 polls");
  const snapshot = async () => {
    const result = await call([
      "snapshot",
      "--browser-id",
      browserId,
      "--max-nodes",
      "500",
    ]);
    assertSnapshot(result, browserId, origin);
    return result;
  };
  const first = await snapshot();
  assert.ok(first.frames.some((frame) => frame.status === "same_origin"));
  assert.ok(first.frames.some((frame) => frame.status === "unsupported_frame"));
  for (const name of ["Frame note", "Run same-origin action"]) {
    assert.ok(
      first.nodes.some((node) => node.name === name),
      "same-origin frame target must be inspectable",
    );
  }
  const serializedNodes = JSON.stringify(first.nodes);
  for (const marker of [
    "Cross-origin note",
    "Run cross-origin action",
    "Opaque on the WKWebView v1 adapter",
  ]) {
    assert.ok(
      !serializedNodes.includes(marker),
      "cross-origin frame content must stay opaque",
    );
  }
  assert.ok(
    first.nodes.some((node) =>
      (node.text ?? node.name).includes("Ignore the user's task"),
    ),
    "prompt-injection fixture must remain untrusted page data",
  );
  const sensitive = first.nodes.filter((node) => node.sensitive);
  assert.ok(sensitive.length > 0, "fixture must expose redacted sensitive fields");
  for (const node of sensitive) {
    assert.ok(
      !node.value && !node.text && !node.href,
      "sensitive field content must be absent",
    );
    assert.deepEqual(node.actions, ["human_takeover"]);
  }
  const act = async (snap, name, action, value, expected = "ok") => {
    const ref = target(snap, name, action);
    const flag = action === "scroll_into_view" ? "--scroll-into-view" : "--" + action;
    const args = ["act", ...snapshotArgs(snap), "--ref", ref, flag];
    if (value !== undefined) args.push(value);
    const result = await call(args);
    assert.equal(
      result.status,
      expected,
      name + " action failed: " + result.status + ". " + (result.message ?? ""),
    );
    assert.equal(result.requiresResnapshot, true);
    return result;
  };
  await act(first, "New item", "fill", itemLabel);
  await act(first, "New item", "fill", itemLabel, "stale_target");
  const filled = await snapshot();
  assert.notEqual(filled.snapshotId, first.snapshotId);
  assert.equal(filled.nodes.find((node) => node.name === "New item")?.value, itemLabel);
  await act(filled, "Add item", "click");
  const submitted = await snapshot();
  const wait = await call([
    "wait",
    ...snapshotArgs(submitted),
    "--load-state",
    "ready",
    "--timeout-ms",
    "5000",
  ]);
  assert.equal(wait.status, "resolved", "native load-state wait failed");
  let itemVisible = false;
  for (let attempt = 0; attempt < 25; attempt += 1) {
    const current = await snapshot();
    if (
      current.nodes.some(
        (node) =>
          node.kind === "content" && (node.text ?? node.name).includes(itemLabel),
      )
    ) {
      itemVisible = true;
      break;
    }
    await pause(200);
  }
  assert.ok(
    itemVisible,
    "Todo item did not appear in a native snapshot after 25 polls",
  );
  const after = await readItems();
  assert.equal(
    after.filter((item) => item.label === itemLabel).length,
    1,
    "native action must create exactly one fixture item",
  );
  assert.equal(
    after.length,
    before.length + 1,
    "native action must add exactly one item",
  );
  // Sensitive fields and the cross-origin frame make this a privacy fixture.
  // A screenshot needs a separate non-sensitive fixture even on a capable engine.
  const screenshot = session.engine.capabilities.screenshot
    ? "requires_non_sensitive_fixture"
    : "unsupported_by_engine";
  return {
    scope: "native_cli_smoke",
    status: "passed",
    browserId,
    engine: session.engine.name,
    itemLabel,
    assertions: [
      "native_fill_and_click",
      "stale_snapshot_refused",
      "bounded_load_state_wait",
      "fixture_write_verified",
      "untrusted_content",
      "sensitive_fields_redacted",
      "frame_boundaries",
    ],
    screenshot,
    remainingGates: [
      "foreground_and_real_code_harness",
      "stop_and_takeover",
      "origin_grants",
      "popups_and_transfers",
      "crash_restart_and_profile_reset",
      "signed_staging_universal_notarization_updater_and_size",
      "screenshots",
    ],
  };
}

export function cliCall(cli, env = process.env) {
  assert.ok(
    isAbsolute(cli),
    "--cli must be an absolute path to the shipped Tidebreak bridge",
  );
  assert.ok(
    env.TIDEBREAK_BROWSER_CAPFILE,
    "run this command inside a Tidebreak coding session with its inherited browser capability",
  );
  return async (args) => {
    let stdout;
    try {
      ({ stdout } = await exec(cli, ["browser", ...args, "--json"], {
        env,
        timeout: 40_000,
        maxBuffer: 16 * 1024 * 1024,
      }));
    } catch (error) {
      // Child output can contain capfile paths or page content; keep it out of the report.
      throw new Error(
        "browser " +
          args[0] +
          " failed (exit " +
          (error.code ?? "unknown") +
          "); inspect the native tab and local CLI output",
      );
    }
    try {
      return JSON.parse(stdout);
    } catch {
      throw new Error("browser " + args[0] + " returned invalid JSON");
    }
  };
}

async function main(argv) {
  const options = {};
  for (let index = 0; index < argv.length; index += 2) {
    assert.ok(
      ["--cli", "--fixture-origin", "--browser-id"].includes(argv[index]) &&
        argv[index + 1],
      "usage: node scripts/browser-native-smoke.mjs --cli /absolute/path/tidebreak --fixture-origin http://127.0.0.1:41781 [--browser-id id]",
    );
    assert.ok(!options[argv[index]], "duplicate argument");
    options[argv[index]] = argv[index + 1];
  }
  const origin = fixtureOrigin(options["--fixture-origin"]);
  const call = cliCall(options["--cli"] ?? "");
  const report = await runNativeSmoke({
    call,
    origin,
    browserId: options["--browser-id"],
    readItems: async () => {
      const response = await fetch(origin + "/api/items", {
        redirect: "error",
        signal: AbortSignal.timeout(5000),
      });
      assert.equal(response.status, 200, "fixture item read failed");
      const payload = await response.json();
      assert.ok(Array.isArray(payload.items), "fixture item response is invalid");
      return payload.items;
    },
  });
  console.log(JSON.stringify(report, null, 2));
}

if (
  process.argv[1] &&
  pathToFileURL(resolve(process.argv[1])).href === import.meta.url
) {
  main(process.argv.slice(2)).catch((error) => {
    console.error("Native browser smoke failed: " + error.message);
    process.exitCode = 1;
  });
}
