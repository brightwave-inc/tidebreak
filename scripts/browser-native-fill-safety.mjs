import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { cliCall, fixtureOrigin } from "./browser-native-smoke.mjs";

export async function runNativeFillSafety({
  call,
  origin,
  browserId: requestedBrowserId,
  pause = (ms) => new Promise((done) => setTimeout(done, ms)),
}) {
  const sessions = (await call(["list"])).sessions.filter((session) =>
    session.visible && session.url && new URL(session.url).origin === origin &&
    (!requestedBrowserId || session.browserId === requestedBrowserId));
  assert.equal(sessions.length, 1, "open and share one visible fixture tab in this session");
  const session = sessions[0];
  assert.equal(session.engine.name, "wk_web_view", "this test requires the native engine");
  const browserId = session.browserId;
  const snapshot = async () => {
    const result = await call(["snapshot", "--browser-id", browserId, "--max-nodes", "500"]);
    assert.equal(result.browserId, browserId);
    assert.equal(result.truncated, false, "the fixture snapshot must be complete");
    return result;
  };
  const report = [];
  for (const fixtureCase of ["replace_on_focus", "steal_focus", "replace_on_select", "steal_focus_on_select"]) {
    const url = origin + "/?nativeFillCase=" + fixtureCase;
    await call(["navigate", "--browser-id", browserId, "--url", url]);
    let ready = false;
    for (let attempt = 0; attempt < 25; attempt += 1) {
      const state = (await call(["list"])).sessions.find((entry) => entry.browserId === browserId);
      assert.ok(state?.visible, "keep the native fixture visible");
      assert.notEqual(state.loadState, "failed", "fixture navigation failed");
      if (state.loadState === "ready" && state.url === url) { ready = true; break; }
      await pause(200);
    }
    assert.ok(ready, "fixture did not become ready after 25 polls");
    const before = await snapshot();
    const fields = before.nodes.filter((node) => node.name === "New item" && node.kind === "interactive");
    assert.equal(fields.length, 1);
    const field = fields[0];
    assert.equal(field.value, "Original fixture text", "race fixture did not initialize");
    assert.ok(field.ref && field.actions.includes("fill"));
    const forbidden = "Must not insert " + randomUUID();
    const action = await call(["act", "--browser-id", browserId,
      "--snapshot-id", before.snapshotId, "--document-epoch", String(before.documentEpoch),
      "--ref", field.ref, "--execution-mode", "foreground", "--fill", forbidden]);
    const expectedStatus = fixtureCase.startsWith("replace_") ? "stale_target" : "unsupported_native";
    assert.equal(action.status, expectedStatus, fixtureCase + " must refuse changed input: " + (action.message ?? ""));
    const live = (await call(["list"])).sessions.find((entry) => entry.browserId === browserId);
    assert.ok(live?.visible, "the same native tab must remain visible after refusal");
    assert.equal(live.url, url);
    assert.equal(live.engine.name, "wk_web_view");
    const after = await snapshot();
    assert.ok(after.nodes.some((node) => (node.text ?? node.name ?? "").includes(
      "Native fill case triggered: " + fixtureCase)), fixtureCase + " must execute its native event handler");
    assert.equal(after.nodes.find((node) => node.name === "New item" && node.kind === "interactive")?.value,
      "Original fixture text", fixtureCase + " changed the original or replacement input");
    assert.equal(after.nodes.find((node) => node.name === "Display name" && node.kind === "interactive")?.value,
      "Ada", fixtureCase + " typed into the field that stole focus");
    assert.ok(!JSON.stringify(after.nodes).includes(forbidden), fixtureCase + " inserted text into the page");
    const verifier = after.nodes.filter((node) => node.name === "Verify native fill values" && node.kind === "interactive");
    assert.equal(verifier.length, 1);
    assert.ok(verifier[0].ref && verifier[0].actions.includes("click"));
    const verified = await call(["act", "--browser-id", browserId,
      "--snapshot-id", after.snapshotId, "--document-epoch", String(after.documentEpoch),
      "--ref", verifier[0].ref, "--execution-mode", "foreground", "--click"]);
    assert.equal(verified.status, "ok", "native value verification click failed");
    const valuesSnapshot = await snapshot();
    const valuesText = valuesSnapshot.nodes.filter((node) => node.kind === "content")
      .map((node) => node.text ?? node.name ?? "").find((text) => text.startsWith("Native fill values: "));
    assert.ok(valuesText, "native verification must report the retained original and current input values");
    assert.deepEqual(JSON.parse(valuesText.slice("Native fill values: ".length)), {
      original: "Original fixture text", current: "Original fixture text", decoy: "Ada",
    }, fixtureCase + " inserted text into an original, replacement, or decoy field");
    report.push({ fixtureCase, status: action.status, noInsertion: true });
  }
  return { scope: "native_fill_safety", status: "passed", browserId, cases: report };
}

async function main(argv) {
  const options = {};
  for (let index = 0; index < argv.length; index += 2) {
    const key = argv[index];
    assert.ok(["--cli", "--fixture-origin", "--browser-id"].includes(key) && argv[index + 1],
      "pass --cli, --fixture-origin, and optionally --browser-id");
    assert.ok(!options[key], "duplicate argument");
    options[key] = argv[index + 1];
  }
  const report = await runNativeFillSafety({
    call: cliCall(options["--cli"] ?? ""),
    origin: fixtureOrigin(options["--fixture-origin"]),
    browserId: options["--browser-id"],
  });
  console.log(JSON.stringify(report, null, 2));
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  main(process.argv.slice(2)).catch((error) => {
    console.error("Native fill safety failed: " + error.message);
    process.exitCode = 1;
  });
}
