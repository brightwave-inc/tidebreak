import assert from "node:assert/strict";
import { test } from "node:test";
import { runNativeFillSafety } from "./browser-native-fill-safety.mjs";

function fixture({ declined = false, runHandler = true } = {}) {
  const origin = "http://127.0.0.1:41781";
  const browserId = "native-fill-fixture";
  const calls = [];
  let fixtureCase;
  let attempted = false;
  let verified = false;
  let snapshotId = 0;
  let url = origin + "/";
  return {
    origin,
    browserId,
    calls,
    call: async (args) => {
      calls.push(args);
      const flag = (key) => args[args.indexOf(key) + 1];
      if (args[0] === "list") return { sessions: [{
        browserId, url, visible: true, loadState: "ready", engine: { name: "wk_web_view" },
      }] };
      if (args[0] === "navigate") {
        url = flag("--url");
        fixtureCase = new URL(url).searchParams.get("nativeFillCase");
        attempted = false;
        verified = false;
        return { browserId, url };
      }
      if (args[0] === "snapshot") return {
        browserId, snapshotId: "snapshot-" + ++snapshotId, documentEpoch: 1, truncated: false,
        nodes: [
          { kind: "interactive", name: "New item", ref: "field", value: "Original fixture text", actions: ["fill"] },
          { kind: "interactive", name: "Display name", ref: "decoy", value: "Ada", actions: ["fill"] },
          { kind: "interactive", name: "Verify native fill values", ref: "verify", actions: ["click"] },
          ...(attempted && runHandler ? [{ kind: "content", text: "Native fill case triggered: " + fixtureCase }] : []),
          ...(verified ? [{ kind: "content", text: "Native fill values: " + JSON.stringify({
            original: "Original fixture text", current: "Original fixture text", decoy: "Ada",
          }) }] : []),
        ],
      };
      if (args[0] === "act") {
        assert.equal(flag("--execution-mode"), "foreground", "every race and verification action must request native input");
        if (declined) return { status: "human_takeover_required", message: "declined" };
        if (args.includes("--fill")) {
          attempted = true;
          return { status: fixtureCase.startsWith("replace_") ? "stale_target" : "unsupported_native" };
        }
        assert.ok(args.includes("--click"));
        verified = true;
        return { status: "ok" };
      }
      throw new Error("unexpected command " + args[0]);
    },
  };
}

test("foreground fill races explicitly request native input for all four cases", async () => {
  const runtime = fixture();
  const report = await runNativeFillSafety(runtime);
  assert.equal(report.status, "passed");
  assert.equal(report.cases.length, 4);
  const actions = runtime.calls.filter((args) => args[0] === "act");
  assert.equal(actions.length, 8);
  assert.equal(actions.filter((args) => args.includes("--fill")).length, 4);
  assert.equal(new Set(actions.map((args) => args[args.indexOf("--snapshot-id") + 1])).size, 8);
});

test("a declined foreground approval stops the runner before verification or another case", async () => {
  const runtime = fixture({ declined: true });
  await assert.rejects(runNativeFillSafety(runtime), /must refuse changed input/);
  assert.equal(runtime.calls.filter((args) => args[0] === "act").length, 1);
  assert.equal(runtime.calls.filter((args) => args[0] === "navigate").length, 1);
});

test("a refusal does not qualify a race unless its fixture handler ran", async () => {
  const runtime = fixture({ runHandler: false });
  await assert.rejects(runNativeFillSafety(runtime), /must execute its native event handler/);
  assert.equal(runtime.calls.filter((args) => args[0] === "act").length, 1);
});
