import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { isAbsolute, resolve } from "node:path";
import { promisify } from "node:util";
import { pathToFileURL } from "node:url";
import { readFile, readdir, writeFile, mkdir, stat } from "node:fs/promises";

const exec = promisify(execFile);

const APP_ID = "dev.tidebreak.ComputerUseFixture";
const WINDOW_TITLE = "Computer Use Fixture";
const MAX_SCREENSHOT_BYTES = 20 * 1024 * 1024;

export const REQUIRED_IDENTIFIERS = [
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
];


function unwrapResult(result, name) {
  if (!result || typeof result !== "object") {
    throw new Error("computer " + name + " returned no result object");
  }
  const outcome = result.outcome ?? result.status ?? "completed";
  if (outcome !== "completed" && outcome !== "ok") {
    const errorCode = result.error_code ?? result.errorCode ?? result.data?.status ?? "unknown";
    const detail =
      result.message ?? result.detail ?? result.error ?? result.data?.message ?? "no failure detail";
    throw new Error(
      "computer " + name + " failed: " + errorCode + " — " + detail,
    );
  }
  const data = result.data;
  if (data?.status && !["ok", "completed"].includes(data.status)) {
    throw new Error("computer " + name + " failed: " + (data.message ?? data.status));
  }
  return data && typeof data === "object" ? { ...result, ...data } : result;
}

function readResultTree(result) {
  let root = result.nodes ?? result.tree ?? result.data?.nodes ?? result.data?.tree;
  if (typeof root === "string") root = JSON.parse(root);
  if (!root) throw new Error("computer_read_app_content returned no accessibility tree");
  const nodes = [];
  const visit = (node) => {
    if (!node || typeof node !== "object") return;
    nodes.push(node);
    for (const child of node.children ?? []) visit(child);
  };
  for (const node of Array.isArray(root) ? root : [root]) visit(node);
  return nodes;
}

export function findTarget(tree, identifier, expectedRole) {
  const candidates = tree.filter((node) => node.identifier === identifier);
  assert.equal(
    candidates.length,
    1,
    "fixture must expose exactly one target " + identifier,
  );
  const node = candidates[0];
  const id = node.element_id ?? node.id;
  const fingerprint = node.element_fingerprint ?? node.fingerprint;
  assert.ok(
    id && fingerprint,
    "target " + identifier + " must carry element_id and fingerprint",
  );
  if (expectedRole) {
    assert.ok(
      (node.role ?? node.type ?? "").toLowerCase().includes(expectedRole.toLowerCase()),
      "target " + identifier + " must have role " + expectedRole,
    );
  }
  return { id, fingerprint, identifier, label: node.label ?? node.name ?? null };
}

export function findFixtureWindow(windows) {
  const owner = (window) => window.bundle_id ?? window.bundleId ?? window.app_id;
  const title = (window) => window.title ?? window.name;
  const visible = windows.filter((window) =>
    window.visible !== false && owner(window) === APP_ID,
  );
  const titled = visible.filter((window) => title(window) === WINDOW_TITLE);
  assert.ok(titled.length <= 1, "fixture window title is ambiguous");
  if (titled.length === 1) return titled[0];

  // macOS can omit window titles before Screen Recording is granted.
  // Accept one explicitly owned window; never guess among several windows.
  const owned = visible.filter((window) => owner(window) === APP_ID);
  if (owned.length === 1 && !title(owned[0])) return owned[0];
  return undefined;
}

export async function readFixtureEvents(fixtureDir, runID) {
  const dir = resolve(fixtureDir, "events", runID);
  const names = (await readdir(dir)).filter((name) => /^\d{6}\.json$/.test(name)).sort();
  assert.ok(names.length > 0, "fixture run has no event files");
  const events = [];
  for (let index = 0; index < names.length; index += 1) {
    const expected = String(index + 1).padStart(6, "0") + ".json";
    assert.equal(
      names[index],
      expected,
      "fixture event sequence must be dense and ordered",
    );
    const record = JSON.parse(await readFile(resolve(dir, names[index]), "utf8"));
    assert.equal(record.sequence, index + 1, "fixture event sequence mismatch");
    assert.equal(record.run_id, runID, "fixture event run id mismatch");
    events.push(record);
  }
  return events;
}

export async function fixtureSnapshots(fixtureDir, runID) {
  const events = await readFixtureEvents(fixtureDir, runID);
  return events.filter((record) => record.event === "state_snapshot");
}

async function latestSnapshot(fixtureDir, runID, fixtureSnapshots) {
  const snapshots = await fixtureSnapshots(fixtureDir, runID);
  assert.ok(snapshots.length > 0, "fixture must publish state snapshots");
  return snapshots[snapshots.length - 1].payload;
}

async function readTree(call, maxNodes = 600) {
  const result = unwrapResult(
    await call("computer_read_app_content", {
      app_id: APP_ID,
      max_depth: 25,
      max_nodes: maxNodes,
    }),
    "computer_read_app_content",
  );
  const tree = readResultTree(result);
  assert.ok(Array.isArray(tree), "accessibility tree must be an array");
  return tree;
}

async function waitForFixture(call, fixtureDir, runID, predicate, label, timeoutMs = 7000, snapshotProvider = fixtureSnapshots) {
  const started = Date.now();
  let lastError;
  while (Date.now() - started < timeoutMs) {
    try {
      const state = await latestSnapshot(fixtureDir, runID, snapshotProvider);
      if (predicate(state)) return state;
      lastError = undefined;
    } catch (error) {
      lastError = error;
    }
    await new Promise((done) => setTimeout(done, 200));
  }
  throw new Error(
    "fixture did not reach " + label + " within " + timeoutMs + "ms" +
    (lastError ? " (" + lastError.message + ")" : ""),
  );
}

function pngDimensions(bytes) {
  assert.ok(
    bytes.length > 24 &&
      bytes[0] === 0x89 &&
      bytes[1] === 0x50 &&
      bytes[2] === 0x4e &&
      bytes[3] === 0x47,
    "captured screenshot must be a PNG file",
  );
  return {
    width: bytes.readUInt32BE(16),
    height: bytes.readUInt32BE(20),
  };
}

async function saveScreenshot(call, fixtureDir, runID, name) {
  const metaDir = resolve(fixtureDir, "screenshots", runID);
  await mkdir(metaDir, { recursive: true });
  const file = resolve(metaDir, name);
  const requestedAt = Date.now();
  const result = unwrapResult(
    await call("computer_capture_screen", { app_id: APP_ID, annotate: false }, { output: file }),
    "computer_capture_screen",
  );
  const images = result.images ?? result.data?.images ?? [];
  const image = result.image_file ? { path: result.image_file, mime_type: "image/png" } : images[0];
  assert.ok(image, "capture must return an image file or image bytes");
  if (result.image_file) {
    assert.equal(resolve(result.image_file), file, "CLI capture must name the requested output file");
    assert.ok(result.image_count > 0, "CLI capture must report a delivered image");
  }
  let bytes;
  if (typeof image.path === "string" && image.path) {
    const file = await stat(image.path);
    assert.ok(file.isFile(), "captured image path must be a regular file");
    assert.ok(file.mtimeMs >= requestedAt - 1000, "captured image path must be updated by this capture");
    assert.ok(file.size > 0 && file.size <= MAX_SCREENSHOT_BYTES, "captured image has an invalid size");
    bytes = await readFile(image.path);
  } else if (typeof image === "string") {
    bytes = Buffer.from(image, "base64");
  } else if (typeof image.base64 === "string") {
    bytes = Buffer.from(image.base64, "base64");
  } else {
    throw new Error("capture returned neither a real image path nor bounded image bytes");
  }
  assert.ok(bytes.length > 0, "captured screenshot is empty");
  assert.ok(
    bytes.length <= MAX_SCREENSHOT_BYTES,
    "captured screenshot exceeds " + MAX_SCREENSHOT_BYTES + " bytes",
  );
  const dimensions = pngDimensions(bytes);
  if (!result.image_file) await writeFile(file, bytes, { mode: 0o600 });
  const sha256 = createHash("sha256").update(bytes).digest("hex");
  const metadata = {
    screenshot: file,
    run_id: runID,
    mime_type: image.mime_type ?? image.mimeType ?? "image/png",
    bytes: bytes.length,
    sha256,
    width: dimensions.width,
    height: dimensions.height,
    captured_at: new Date().toISOString(),
  };
  await writeFile(file + ".json", JSON.stringify(metadata, null, 2) + "\n", { mode: 0o600 });
  return metadata;
}

async function assertSnapshotValue(state, key, expected, label) {
  assert.equal(state[key], expected, label + " must be visible in the authoritative fixture snapshot");
}

/**
 * The complete native acceptance flow. `call` is the only seam to the
 * Tidebreak computer CLI; injected fake CLI calls are used here solely to
 * validate failure handling in the runner's own tests.
 */
export async function runNativeSmoke({
  call,
  fixtureDir,
  runID,
  appPath,
  secondWindow = false,
  verifyReset = true,
  waitTimeoutMs = 7000,
  pause = (ms) => new Promise((done) => setTimeout(done, ms)),
  readEvents = readFixtureEvents,
  snapshots = fixtureSnapshots,
}) {
  const invoke = call;
  call = async (name, args, options) => unwrapResult(await invoke(name, args, options), name);
  fixtureDir = resolve(fixtureDir);
  assert.ok(runID && /^[A-Za-z0-9._-]{1,120}$/.test(runID), "valid --run-id required");
  assert.ok(appPath && isAbsolute(appPath), "--app-path must be an absolute path to the fixture app bundle");

  const waitFor = (predicate, label, timeoutMs) =>
    waitForFixture(call, fixtureDir, runID, predicate, label, timeoutMs ?? waitTimeoutMs, snapshots);
  const currentSnapshot = async () => {
    const records = await snapshots(fixtureDir, runID);
    assert.ok(records.length > 0, "fixture must publish state snapshots");
    return records[records.length - 1].payload;
  };

  // The fixture must already be launched with this run id (or be launchable by
  // the parent integration). Launching is a real native capability; a compile
  // or a claimed success string is never enough.
  const launch = unwrapResult(
    await call("computer_launch_app", {
      execution_mode: "foreground",
      app_id: APP_ID,
    }),
    "computer_launch_app",
  );
  assert.ok(launch.outcome === undefined || launch.outcome === "completed", "launch must complete");

  let mainWindow;
  for (let attempt = 0; attempt < 40; attempt += 1) {
    const result = unwrapResult(
      await call("computer_list_windows", { app_id: APP_ID }),
      "computer_list_windows",
    );
    mainWindow = findFixtureWindow(result.windows ?? result.data?.windows ?? []);
    if (mainWindow) break;
    await pause(250);
  }
  assert.ok(mainWindow, "fixture window did not appear unambiguously after launch");
  const mainWindowId = mainWindow?.window_id ?? mainWindow?.id;
  assert.ok(mainWindowId !== undefined, "fixture window must expose a window id");

  const baseline = await readEvents(fixtureDir, runID);
  assert.ok(
    baseline.some((record) => record.event === "launch_ready"),
    "fixture must write launch_ready before acceptance",
  );
  const before = await currentSnapshot();
  assert.equal(before.submission_count ?? 0, 0, "fixture must start unsubmitted");

  const initialTree = await readTree(call);
  for (const identifier of REQUIRED_IDENTIFIERS) {
    findTarget(initialTree, identifier);
  }

  const beforeScreenshot = await saveScreenshot(call, fixtureDir, runID, "before-submit.png");
  const text = "Native acceptance " + runID;
  const typeTarget = findTarget(initialTree, "fixture-text-input");
  await call("computer_click", {
    execution_mode: "foreground",
    app_id: APP_ID,
    element_id: typeTarget.id, element_fingerprint: typeTarget.fingerprint,
  });
  await call("computer_type_text", {
    execution_mode: "foreground",
    app_id: APP_ID,
    text,
    element_id: typeTarget.id, element_fingerprint: typeTarget.fingerprint,
  });
  await waitFor((state) => state.text_value === text, "text entry");
  let events = await readEvents(fixtureDir, runID);
  assert.ok(
    events.some((record) => record.event === "text_entry" && record.payload.value === text),
    "native typing must write the entered text into the fixture state",
  );

  const addTarget = findTarget(await readTree(call), "fixture-add-button");
  await call("computer_click", {
    execution_mode: "foreground",
    app_id: APP_ID,
    element_id: addTarget.id, element_fingerprint: addTarget.fingerprint,
  });
  await waitFor((state) => state.submission_count === 1, "one submission");
  events = await readEvents(fixtureDir, runID);
  const submissions = events.filter((record) => record.event === "submission");
  assert.equal(submissions.length, 1, "native action must create exactly one fixture submission");
  assert.equal(submissions[0].payload.count, 1, "submission count must be exactly one");
  assert.equal(submissions[0].payload.value, text, "submission must contain the typed text");
  const postSubmit = await currentSnapshot();
  await assertSnapshotValue(postSubmit, "submission_count", 1, "submission count");

  const screenshot = await saveScreenshot(call, fixtureDir, runID, "after-submit.png");
  assert.ok(screenshot.width >= 640 && screenshot.height >= 480, "screenshot must be desktop scale");
  assert.ok(screenshot.bytes >= 8 * 1024, "screenshot must contain real pixels");
  assert.notEqual(screenshot.sha256, beforeScreenshot.sha256, "capture must show a change after submission");

  await call("computer_focus_window", { execution_mode: "foreground", app_id: APP_ID, window_id: mainWindowId });

  const treeForSelect = await readTree(call);
  const dropdown = findTarget(treeForSelect, "fixture-dropdown");
  await call("computer_click", {
    execution_mode: "foreground",
    app_id: APP_ID,
    element_id: dropdown.id, element_fingerprint: dropdown.fingerprint,
  });
  await call("computer_key_press", { execution_mode: "foreground", app_id: APP_ID, key: "down" });
  await call("computer_key_press", { execution_mode: "foreground", app_id: APP_ID, key: "return" });
  await waitFor((state) => state.dropdown === "Second", "dropdown selection");
  await assertSnapshotValue(await currentSnapshot(), "dropdown", "Second", "dropdown");

  const checkbox = findTarget(await readTree(call), "fixture-checkbox");
  await call("computer_click", {
    execution_mode: "foreground",
    app_id: APP_ID,
    element_id: checkbox.id, element_fingerprint: checkbox.fingerprint,
  });
  await waitFor((state) => state.checkbox === true, "checkbox checked");
  await assertSnapshotValue(await currentSnapshot(), "checkbox", true, "checkbox");

  const hover = findTarget(await readTree(call), "fixture-hover-area");
  const hoverResult = unwrapResult(
    await call("computer_hover", {
      execution_mode: "foreground",
      app_id: APP_ID,
      element_id: hover.id, element_fingerprint: hover.fingerprint,
    }),
    "computer_hover",
  );
  assert.ok(hoverResult, "hover must complete");
  await waitFor((state) => state.hovered === true, "hover");
  await assertSnapshotValue(await currentSnapshot(), "hovered", true, "hover");

  const dragTree = await readTree(call);
  const source = findTarget(dragTree, "fixture-drag-item");
  const destination = findTarget(dragTree, "fixture-drop-target");
  await call("computer_drag", {
    execution_mode: "foreground",
    app_id: APP_ID,
    from: { element_id: source.id, element_fingerprint: source.fingerprint },
    to: { element_id: destination.id, element_fingerprint: destination.fingerprint },
    duration_ms: 400,
  });
  await waitFor((state) => state.drag_dropped === true, "drag drop");
  const dragState = await currentSnapshot();
  await assertSnapshotValue(dragState, "drag_dropped", true, "drag");
  assert.equal(dragState.drag_target, "fixture-drop-target", "drag must land on the drop target");

  const scrollTarget = findTarget(await readTree(call), "fixture-scroll-area");
  await call("computer_scroll", {
    execution_mode: "foreground",
    app_id: APP_ID,
    element_id: scrollTarget.id, element_fingerprint: scrollTarget.fingerprint,
    dx: 0,
    dy: 180,
  });
  await waitFor((state) => state.scroll_offset > 0, "scroll evidence");
  const scrollSnap = await currentSnapshot();
  assert.ok(
    typeof scrollSnap.scroll_offset === "number" && scrollSnap.scroll_offset > 0,
    "scroll must move the fixture content",
  );

  const delayed = findTarget(await readTree(call), "fixture-delayed-button");
  await call("computer_click", {
    execution_mode: "foreground",
    app_id: APP_ID,
    element_id: delayed.id, element_fingerprint: delayed.fingerprint,
  });
  await call("computer_wait", { seconds: 1.0 });
  await waitFor((state) => state.delayed_status === "completed", "delayed transition", 6000);
  await assertSnapshotValue(await currentSnapshot(), "delayed_status", "completed", "delayed transition");

  await call("computer_resize_window", {
    execution_mode: "foreground",
    app_id: APP_ID,
    window_id: mainWindowId,
    width: 1040,
    height: 820,
  });
  await waitFor((state) => state.window_size?.width >= 1040, "window resize");
  const resized = await currentSnapshot();
  assert.ok(
    resized.window_size && resized.window_size.width >= 1040 && resized.window_size.height >= 820,
    "resize must change the actual window geometry",
  );

  if (secondWindow) {
    const secondButton = findTarget(await readTree(call), "fixture-second-window-button");
    await call("computer_click", {
      execution_mode: "foreground",
      app_id: APP_ID,
      element_id: secondButton.id, element_fingerprint: secondButton.fingerprint,
    });
    await waitFor((state) => state.second_window_open === true, "second window");
    const twoWindows = (unwrapResult(
      await call("computer_list_windows", { app_id: APP_ID }),
      "computer_list_windows",
    )).windows ?? [];
    assert.ok(twoWindows.length >= 2, "second fixture window must be listed");
    await call("computer_focus_window", { execution_mode: "foreground", app_id: APP_ID, window_id: mainWindowId });
    const closeButton = findTarget(await readTree(call), "fixture-second-window-button");
    await call("computer_click", {
      execution_mode: "foreground",
      app_id: APP_ID,
      element_id: closeButton.id, element_fingerprint: closeButton.fingerprint,
    });
    await waitFor((state) => state.second_window_open === false, "second window closed");
  }

  let resetVerified = null;
  if (verifyReset) {
    assert.equal((await currentSnapshot()).submission_count, 1, "later actions must not submit again");
    const resetButton = findTarget(await readTree(call), "fixture-reset-button");
    await call("computer_click", {
      execution_mode: "foreground",
      app_id: APP_ID,
      element_id: resetButton.id, element_fingerprint: resetButton.fingerprint,
    });
    await waitFor((state) => !!state.run_id && state.run_id !== runID, "reset");
    const resetState = await currentSnapshot();
    const afterReset = await readEvents(fixtureDir, runID);
    const requested = afterReset.find((record) => record.event === "reset_requested");
    assert.ok(requested && requested.payload.new_run_id, "fixture reset must record the new run id");
    assert.equal(resetState.run_id, requested.payload.new_run_id, "reset must switch to a fresh run");
    for (const [key, value] of Object.entries({
      submission_count: 0,
      text_value: "",
      dropdown: "First",
      checkbox: false,
      hovered: false,
      drag_dropped: false,
      delayed_status: "idle",
      second_window_open: false,
      scroll_offset: 0,
    })) {
      assert.equal(resetState[key], value, "reset must clear " + key);
    }
    resetVerified = resetState.run_id;
  }

  const screenshots = [beforeScreenshot, screenshot];
  return {
    scope: "computer_use_native_smoke",
    execution_mode: "foreground",
    status: "passed",
    app_id: APP_ID,
    run_id: runID,
    fixture_dir: fixtureDir,
    window_title: WINDOW_TITLE,
    screenshots,
    reset_run_id: resetVerified,
    assertions: [
      "native_launch",
      "native_text_type_and_exactly_one_submission",
      "native_dropdown_and_checkbox",
      "native_hover_and_drag",
      "native_scroll",
      "bounded_wait_for_delayed_state",
      "native_window_resize",
      "fixture_side_effect_written_not_tool_success",
      "screenshot_file_and_metadata",
    ],
    remainingGates: [
      "stop",
      "takeover",
      "approval_surface",
      "concurrent_ownership",
      "uncertain_outcome_recovery",
    ],
  };
}

/**
 * One adapter function owns every Tidebreak CLI invocation so the final
 * computer-use CLI shape can be adjusted here without touching the flow.
 * The runner's own tests inject a fake `call` and never invoke this.
 */
export function cliCall(cli, environment = process.env, execute = exec) {
  assert.ok(
    isAbsolute(cli),
    "--cli must be an absolute path to the bundled Tidebreak computer CLI",
  );
  return async (name, args, { output } = {}) => {
    const argv = ["computer", name, "--json", JSON.stringify(args)];
    if (output) {
      assert.ok(isAbsolute(output), "capture output must be an absolute path");
      argv.push("--output", output);
    }
    let stdout;
    try {
      ({ stdout } = await execute(cli, argv, {
        env: environment,
        timeout: 90_000,
        maxBuffer: 64 * 1024 * 1024,
      }));
    } catch (error) {
      throw new Error(
        "computer " + name + " failed (exit " + (error.code ?? "unknown") + "); inspect native CLI output on the host",
      );
    }
    try {
      return JSON.parse(stdout);
    } catch {
      throw new Error("computer " + name + " returned invalid JSON");
    }
  };
}

async function main(argv) {
  const options = {};
  const valueFlags = new Set(["--cli", "--fixture-dir", "--run-id", "--app-path"]);
  for (let index = 0; index < argv.length; index += 1) {
    if (argv[index] === "--second-window") {
      assert.ok(!options["--second-window"], "duplicate argument");
      options["--second-window"] = true;
      continue;
    }
    assert.ok(
      valueFlags.has(argv[index]) && argv[index + 1],
      "usage: node scripts/computer-use-native-smoke.mjs --cli <absolute-path> --fixture-dir <state-dir> --run-id <id> --app-path <absolute-app-bundle> [--second-window]",
    );
    assert.ok(!options[argv[index]], "duplicate argument");
    options[argv[index]] = argv[index + 1];
    index += 1;
  }
  const call = options["--cli"] ? cliCall(resolve(options["--cli"])) : undefined;
  const report = await runNativeSmoke({
    call: call ?? (() => { throw new Error("no CLI adapter configured"); }),
    fixtureDir: options["--fixture-dir"],
    runID: options["--run-id"],
    appPath: options["--app-path"],
    secondWindow: options["--second-window"] !== undefined,
  });
  console.log(JSON.stringify(report, null, 2));
}

if (
  process.argv[1] &&
  pathToFileURL(resolve(process.argv[1])).href === import.meta.url
) {
  main(process.argv.slice(2)).catch((error) => {
    console.error("Native computer-use smoke failed: " + error.message);
    process.exitCode = 1;
  });
}
