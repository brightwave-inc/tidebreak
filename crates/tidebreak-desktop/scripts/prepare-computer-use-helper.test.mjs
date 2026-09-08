import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { stageComputerUseHelper } from "./prepare-computer-use-helper.mjs";

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), "tidebreak-helper-build-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const calls = [];
  const run = (command, args) => {
    calls.push([command, ...args]);
    if (command === "swift") {
      const arch = args[args.indexOf("--arch") + 1];
      const output = join(root, "swift", arch);
      mkdirSync(output, { recursive: true });
      writeFileSync(join(output, "tidebreak-cu-helper"), arch);
      if (args.includes("--show-bin-path")) return `${output}\n`;
    }
    if (command === "lipo") {
      writeFileSync(args[args.indexOf("-output") + 1], "universal");
    }
    if (command === "security") return '1) ABC "Apple Development: Example (TEAM)"';
    return "";
  };
  return {
    calls, run,
    options: { desktopDir: join(root, "desktop"), workspaceDir: root, targetRoot: join(root, "target"), triple: "aarch64-apple-darwin", release: false },
  };
}

test("stages and signs the Swift binary where the broker resolves it", (t) => {
  const { options, calls, run } = fixture(t);
  const resource = stageComputerUseHelper(options, { run, platform: "darwin", env: {} });
  assert.equal(resource, join(options.desktopDir, "resources/host-broker/tidebreak-cu-helper"));
  assert.equal(readFileSync(resource, "utf8"), "arm64");
  assert.equal(statSync(resource).mode & 0o777, 0o755);
  assert.equal(readFileSync(join(options.desktopDir, "binaries/tidebreak-cu-helper-aarch64-apple-darwin"), "utf8"), "arm64");
  assert.deepEqual(calls.find(([command]) => command === "codesign"), ["codesign", "--force", "--sign", "Apple Development: Example (TEAM)", "--identifier", "io.brightwave.tidebreak.cu-helper.dev", resource]);
});

test("universal builds package both architectures and defer release signing", (t) => {
  const { options, calls, run } = fixture(t);
  const resource = stageComputerUseHelper({ ...options, triple: "universal-apple-darwin", release: true }, { run, platform: "darwin", env: {} });
  assert.equal(readFileSync(resource, "utf8"), "universal");
  assert.equal(calls.filter(([command]) => command === "lipo").length, 1);
  assert.ok(calls.some((call) => call.includes("arm64")));
  assert.ok(calls.some((call) => call.includes("x86_64")));
  assert.equal(calls.some(([command]) => command === "codesign"), false);
});

test("non-macOS targets do not invoke Swift; macOS cross-builds fail clearly", (t) => {
  const { options, calls, run } = fixture(t);
  stageComputerUseHelper({ ...options, triple: "x86_64-pc-windows-msvc" }, { run, platform: "linux", env: {} });
  assert.equal(calls.length, 0);
  assert.throws(() => stageComputerUseHelper(options, { run, platform: "linux", env: {} }), /on macOS with Xcode/);
});

test("a failed Swift build never packages an old helper", (t) => {
  const { options } = fixture(t);
  assert.throws(() => stageComputerUseHelper(options, {
    platform: "darwin", env: {}, run() { throw new Error("Swift build failed"); },
  }), /Swift build failed/);
});
