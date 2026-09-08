import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdir, mkdtemp, writeFile, rm } from "node:fs/promises";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

const exec = promisify(execFile);
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { randomUUID } from "node:crypto";
import { deflateSync, crc32 } from "node:zlib";
import {
  cliCall,
  readFixtureEvents,
  runNativeSmoke,
  REQUIRED_IDENTIFIERS,
} from "./computer-use-native-smoke.mjs";

const APP_ID = "dev.tidebreak.ComputerUseFixture";
const RUN_ID = "acceptance-run";

function makeState(overrides = {}) {
  return {
    app_id: APP_ID,
    title: "Computer Use Fixture",
    run_id: RUN_ID,
    submission_count: 0,
    text_value: "",
    dropdown: "First",
    checkbox: false,
    hovered: false,
    drag_dropped: false,
    drag_target: "none",
    delayed_status: "idle",
    second_window_open: false,
    scroll_offset: 0,
    window_size: { width: 920, height: 760 },
    ...overrides,
  };
}

function fakePng(width = 640, height = 480, seed = 0) {
  const raw = Buffer.alloc(height * (1 + width * 3));
  for (let y = 0; y < height; y += 1) {
    const row = y * (1 + width * 3);
    raw[row] = 0; // filter: None
    for (let x = 0; x < width; x += 1) {
      const offset = row + 1 + x * 3;
      raw[offset] = (x * 7 + y * 13 + seed) % 256;
      raw[offset + 1] = (x * 11 + y * 5) % 256;
      raw[offset + 2] = (x * 3 + y * 17) % 256;
    }
  }
  const idat = deflateSync(raw, { level: 0 });
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8;
  ihdr[9] = 2;
  const chunk = (type, data) => {
    const name = Buffer.from(type, "ascii");
    const body = Buffer.concat([name, data]);
    const out = Buffer.alloc(8 + body.length + 4);
    out.writeUInt32BE(data.length, 0);
    body.copy(out, 4);
    out.writeUInt32BE(crc32(body) >>> 0, 4 + body.length);
    return out;
  };
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", idat),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

function nodeEntry(identifier, id, role = "AXGroup", extra = {}) {
  return {
    identifier,
    element_id: id,
    element_fingerprint: "fp-" + id,
    role,
    label: identifier,
    ...extra,
  };
}

function tree() {
  return [
    nodeEntry("fixture-text-input", "e-input", "AXTextField"),
    nodeEntry("fixture-add-button", "e-add", "AXButton"),
    nodeEntry("fixture-submission-count", "e-count", "AXStaticText"),
    nodeEntry("fixture-dropdown", "e-dropdown", "AXPopUpButton"),
    nodeEntry("fixture-checkbox", "e-checkbox", "AXCheckBox"),
    nodeEntry("fixture-hover-area", "e-hover", "AXGroup"),
    nodeEntry("fixture-scroll-area", "e-scroll", "AXScrollArea"),
    nodeEntry("fixture-drag-item", "e-drag", "AXGroup"),
    nodeEntry("fixture-drop-target", "e-drop-target", "AXGroup"),
    nodeEntry("fixture-delayed-button", "e-delayed", "AXButton"),
    nodeEntry("fixture-reset-button", "e-reset", "AXButton"),
  ];
}

function nativeFixture(options = {}) {
  const state = makeState();
  const events = [];
  const calls = [];
  let sequence = 0;
  const writeEvent = (event, payload) => {
    sequence += 1;
    events.push({ event, run_id: RUN_ID, sequence, payload });
  };
  const writeSnapshot = () => writeEvent("state_snapshot", { ...state });
  writeEvent("launch_ready", { app_id: APP_ID });
  writeSnapshot();

  const originalCall = async (name, args) => {
    calls.push([name, args]);
    switch (name) {
      case "computer_launch_app":
        assert.deepEqual(Object.keys(args), ["app_id"], "launch only accepts a registered bundle id");
        if (options.failLaunch) throw new Error("launch rejected");
        return { outcome: "completed" };
      case "computer_list_windows":
        return { outcome: "completed", data: { windows: [{ window_id: 1, title: "Computer Use Fixture", visible: true }] } };
      case "computer_focus_window":
        return { outcome: "completed" };
      case "computer_read_app_content": {
        let nodes = tree();
        if (options.missingCheckbox) {
          nodes = nodes.filter((node) => node.identifier !== "fixture-checkbox");
        }
        return { outcome: "completed", data: { status: "ok", tree: JSON.stringify({ children: nodes }) } };
      }
      case "computer_capture_screen":
        return { images: [{ mime_type: "image/png", base64: fakePng(640, 480, options.staleScreenshot ? 0 : state.submission_count).toString("base64") }] };
      case "computer_type_text":
        state.text_value = args.text;
        writeEvent("text_entry", { value: args.text });
        writeSnapshot();
        return { outcome: "completed" };
      case "computer_click": {
        const identifier = tree().find((node) => node.element_id === args.element_id)?.identifier;
        if (identifier === "fixture-add-button" && !options.suppressAdd) {
          state.submission_count += 1;
          writeEvent("submission", { count: state.submission_count, value: state.text_value });
          state.text_value = "";
          writeSnapshot();
        }
        if (identifier === "fixture-checkbox") {
          state.checkbox = !state.checkbox;
          writeEvent("checkbox_toggle", { checked: state.checkbox });
          writeSnapshot();
        }
        if (identifier === "fixture-delayed-button") {
          state.delayed_status = "completed";
          writeEvent("delayed_status", { status: "completed" });
          writeSnapshot();
        }
        if (identifier === "fixture-reset-button") {
          Object.assign(state, makeState({ run_id: "fresh-run" }));
          writeEvent("reset_requested", { new_run_id: "fresh-run" });
          writeSnapshot();
          events.push({ event: "reset_completed", run_id: RUN_ID, sequence: ++sequence, payload: { new_run_id: "fresh-run" } });
        }
        return { outcome: "completed" };
      }
      case "computer_key_press":
        if (args.key === "down") state.dropdown = "Second";
        if (args.key === "return" && state.dropdown === "Second") {
          writeEvent("dropdown_selection", { value: "Second" });
          writeSnapshot();
        }
        return { outcome: "completed" };
      case "computer_hover":
        state.hovered = true;
        writeEvent("hover_status", { hovered: true });
        writeSnapshot();
        return { outcome: "completed" };
      case "computer_drag":
        assert.ok(args.from?.element_id && args.to?.element_id, "drag uses from/to targets");
        state.drag_dropped = true;
        state.drag_target = "fixture-drop-target";
        writeEvent("drag_started", { target: "fixture-drag-item" });
        writeEvent("drag_dropped", { target: "fixture-drop-target" });
        writeSnapshot();
        return { outcome: "completed" };
      case "computer_scroll":
        state.scroll_offset = 180;
        writeEvent("scroll", { content_y: 180 });
        writeSnapshot();
        return { outcome: "completed" };
      case "computer_wait":
        assert.deepEqual(args, { seconds: 1.0 });
        return { outcome: "completed" };
      case "computer_resize_window":
        if (options.muteForce) state.force = (state.force ?? 0) + 1;
        state.window_size = { width: 1040, height: 820 };
        writeEvent("window_resized", { width: 1040, height: 820 });
        writeSnapshot();
        return { outcome: "completed" };
      default:
        throw new Error("unexpected computer call " + name);
    }
  };

  const call = options.failHover
    ? async (name, args) => {
        if (name === "computer_hover") throw new Error("computer_hover failed (exit 2)");
        return originalCall(name, args);
      }
    : originalCall;

  return {
    state,
    events,
    calls,
    call,
    readEvents: async (directory, runID) => {
      assert.equal(runID, RUN_ID);
      void directory;
      return events.map((event) => structuredClone(event));
    },
    snapshots: async (directory, runID) => {
      assert.equal(runID, RUN_ID);
      void directory;
      return events
        .filter((event) => event.event === "state_snapshot")
        .map((event) => structuredClone(event));
    },
  };
}

function smokeOptions(fixture, extra = {}) {
  return {
    call: fixture.call,
    fixtureDir: join(resolve(tmpdir()), "cu-fixture-" + randomUUID()),
    runID: RUN_ID,
    appPath: "/Fixture.app",
    readEvents: fixture.readEvents,
    snapshots: fixture.snapshots,
    ...extra,
  };
}

test("a failed launch fails loudly before any interaction", async () => {
  const fixture = nativeFixture({ failLaunch: true });
  await assert.rejects(
    runNativeSmoke(smokeOptions(fixture)),
    /launch rejected|computer_launch_app failed/,
  );
  assert.equal(fixture.calls.length, 1);
  assert.equal(fixture.calls[0][0], "computer_launch_app");
});

test("missing acceptance identifiers fail before any mutation", async () => {
  const fixture = nativeFixture({ missingCheckbox: true });
  await assert.rejects(
    runNativeSmoke(smokeOptions(fixture)),
    /exactly one target fixture-checkbox/,
  );
  assert.ok(
    fixture.calls.every(([name]) => name !== "computer_click"),
    "no act call should occur before every target is present",
  );
});

test("targets are re-read between every act", async () => {
  const fixture = nativeFixture();
  let reads = 0;
  const original = fixture.call;
  fixture.call = async (name, args) => {
    if (name === "computer_read_app_content") reads += 1;
    return original(name, args);
  };
  const report = await runNativeSmoke(smokeOptions(fixture));
  assert.equal(report.status, "passed");
  assert.ok(reads >= 5, "the runner must re-read the accessibility tree between acts");
});

test("a claimed successful action without the fixture write fails acceptance", async () => {
  const fixture = nativeFixture({ suppressAdd: true });
  await assert.rejects(
    runNativeSmoke(smokeOptions(fixture, { waitTimeoutMs: 150 })),
    /create exactly one fixture submission|did not reach one submission/,
  );
});

test("an injected capability refusal fails the whole run loudly", async () => {
  const fixture = nativeFixture({ failHover: true });
  await assert.rejects(
    runNativeSmoke(smokeOptions(fixture)),
    /computer_hover failed/,
  );
});

test("the adapter requires an absolute bundled binary and hides raw CLI output", async () => {
  assert.throws(() => cliCall("tidebreak", {}), /absolute path/);
  const call = cliCall(process.execPath, {});
  await assert.rejects(
    call("computer_list_windows", { app_id: APP_ID }),
    /computer_list_windows failed/,
  );
});

test("fixture event files must be dense, ordered, and correctly run-tagged", async () => {
  const root = join(resolve(tmpdir()), "cu-events-" + randomUUID());
  const dir = join(root, "events", "run-a");
  await mkdir(dir, { recursive: true });
  await writeFile(join(dir, "000001.json"), JSON.stringify({ event: "launch_ready", run_id: "run-a", sequence: 1 }));
  await writeFile(join(dir, "000003.json"), JSON.stringify({ event: "launch_ready", run_id: "run-a", sequence: 3 }));
  await assert.rejects(readFixtureEvents(root, "run-a"), /dense and ordered/);
  await writeFile(join(dir, "000002.json"), JSON.stringify({ event: "launch_ready", run_id: "wrong-run", sequence: 2 }));
  await assert.rejects(readFixtureEvents(root, "run-a"), /run id mismatch/);
});

test("the smoke report carries real evidence and non-mocked remaining gates", async () => {
  const fixture = nativeFixture();
  const dir = join(resolve(tmpdir()), "cu-report-" + randomUUID());
  const report = await runNativeSmoke(smokeOptions(fixture, { fixtureDir: dir }));
  assert.equal(report.status, "passed");
  assert.equal(report.screenshots.length, 2);
  assert.ok(report.screenshots[0].bytes >= 8 * 1024, "screenshot must have real pixels");
  assert.ok(report.screenshots[0].width >= 640 && report.screenshots[0].height >= 480);
  assert.equal(report.reset_run_id, "fresh-run");
  for (const gate of ["stop", "takeover", "approval_surface", "concurrent_ownership", "uncertain_outcome_recovery"]) {
    assert.ok(report.remainingGates.includes(gate), "missing remaining gate " + gate);
  }
});

test("the adapter emits the expected shared tool names", async () => {
  const fixture = nativeFixture();
  await runNativeSmoke(smokeOptions(fixture));
  const names = new Set(fixture.calls.map(([name]) => name));
  for (const name of [
    "computer_launch_app",
    "computer_focus_window",
    "computer_list_windows",
    "computer_read_app_content",
    "computer_capture_screen",
    "computer_click",
    "computer_type_text",
    "computer_key_press",
    "computer_scroll",
    "computer_hover",
    "computer_drag",
    "computer_resize_window",
    "computer_wait",
  ]) {
    assert.ok(names.has(name), "runner must call " + name);
  }
  assert.ok(!names.has("computer_exec") && !names.has("browser"), "runner must not escape the adapter");
});

test("required identifier list is the fixture's acceptance contract", () => {
  assert.deepEqual(
    REQUIRED_IDENTIFIERS,
    [
      "fixture-text-input",
      "fixture-add-button",
      "fixture-submission-count",
      "fixture-dropdown",
      "fixture-checkbox",
      "fixture-hover-area",
      "fixture-scroll-area",
      "fixture-drag-item",
      "fixture-drop-target",
      "fixture-delayed-button",
      "fixture-reset-button",
    ],
  );
});


test("every native refusal fails even when the fixture can reach the target state", async () => {
  const fixture = nativeFixture();
  const original = fixture.call;
  fixture.call = async (name, args) => {
    if (name === "computer_wait") {
      return { outcome: "failed", error_code: "unsupported", message: "wait is unavailable" };
    }
    return original(name, args);
  };
  await assert.rejects(runNativeSmoke(smokeOptions(fixture)), /computer_wait failed.*unsupported/);
});

test("reset must clear the state rather than only replace its run id", async () => {
  const fixture = nativeFixture();
  const original = fixture.call;
  fixture.call = async (name, args) => {
    const result = await original(name, args);
    if (name === "computer_click" && args.element_id === "e-reset") {
      fixture.events.findLast((event) => event.event === "state_snapshot").payload.submission_count = 1;
    }
    return result;
  };
  await assert.rejects(runNativeSmoke(smokeOptions(fixture)), /reset must clear submission_count/);
});

test("the actual Swift EventStore writes the smoke schema and preserves reset ownership", {
  skip: process.platform !== "darwin",
}, async () => {
  const directory = await mkdtemp(join(tmpdir(), "cu-event-store-"));
  try {
    const source = fileURLToPath(new URL("./computer-use-fixture/EventStore.swift", import.meta.url));
    const driver = join(directory, "main.swift");
    const binary = join(directory, "event-store-test");
    await writeFile(driver, `import Foundation
let root = URL(fileURLWithPath: CommandLine.arguments[1])
let store = try EventStore(fixtureDirectory: root, runID: "contract-run")
store.write("launch_ready", payload: ["app_id": "dev.tidebreak.ComputerUseFixture"])
store.write("state_snapshot", payload: ["submission_count": 1, "run_id": "contract-run"])
store.write("reset_requested", payload: ["new_run_id": "fresh-run"])
store.write("state_snapshot", payload: ["submission_count": 0, "run_id": "fresh-run"])
`);
    await exec("swiftc", [source, driver, "-o", binary], { timeout: 60_000 });
    await exec(binary, [directory], { timeout: 10_000 });
    const events = await readFixtureEvents(directory, "contract-run");
    assert.equal(events.length, 4);
    assert.equal(events[0].payload.app_id, APP_ID);
    assert.equal(events[1].payload.submission_count, 1);
    assert.equal(events[3].run_id, "contract-run");
    assert.equal(events[3].payload.run_id, "fresh-run");
    assert.equal(events[3].payload.submission_count, 0);
    await assert.rejects(exec(binary, [directory], { timeout: 10_000 }), /already has fixture events/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});


test("a stale screenshot repeated across the submission fails acceptance", async () => {
  const fixture = nativeFixture({ staleScreenshot: true });
  await assert.rejects(runNativeSmoke(smokeOptions(fixture)), /capture must show a change after submission/);
});
