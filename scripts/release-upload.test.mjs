import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const workflow = readFileSync(
  new URL("../.github/workflows/release.yml", import.meta.url),
  "utf8",
);
const step = workflow.match(
  /      - name: Attach or verify the GitHub Release downloads\n[\s\S]*?        run: \|\n([\s\S]*?)(?=\n  finalize_release:)/,
)?.[1];
assert.ok(step, "release upload step must exist");
const script = step.replace(/^          /gm, "");

function upload({ failAttempts = 0, draft = true, incomplete = false } = {}) {
  const dir = mkdtempSync(join(tmpdir(), "tidebreak-release-upload-"));
  try {
    mkdirSync(join(dir, "bin"));
    mkdirSync(join(dir, "downloads"));
    for (const name of ["one.dmg", "two.deb"])
      writeFileSync(join(dir, "downloads", name), name);
    writeFileSync(
      join(dir, "bin", "gh"),
      `#!/usr/bin/env node
const fs = require("node:fs");
const { spawnSync } = require("node:child_process");
const args = process.argv.slice(2);
if (args[0] === "release") {
  const file = args[3];
  fs.appendFileSync("calls", file + "\\n");
  const attempts = fs.readFileSync("calls", "utf8").trim().split("\\n").filter(x => x === file).length;
  if (file.endsWith("two.deb") && attempts <= Number(process.env.FAIL_ATTEMPTS)) {
    console.error("HTTP 500: Error saving asset");
    process.exit(1);
  }
} else if (args[0] === "api") {
  const assets = [
    {name:"one.dmg",state:"uploaded"},
    {name:"two.deb",state:process.env.INCOMPLETE === "true" ? "starter" : "uploaded"}
  ];
  const result = spawnSync("jq", ["-r", args[args.indexOf("--jq") + 1]], {input:JSON.stringify(assets),encoding:"utf8"});
  process.stdout.write(result.stdout);
  process.exit(result.status ?? 1);
} else {
  process.exit(2);
}
`,
      { mode: 0o755 },
    );
    writeFileSync(
      join(dir, "bin", "sleep"),
      "#!/bin/sh\nprintf '%s\\n' \"$1\" >> delays\n",
      { mode: 0o755 },
    );
    const result = spawnSync("bash", ["-e", "-c", script], {
      cwd: dir,
      encoding: "utf8",
      env: {
        ...process.env,
        PATH: `${join(dir, "bin")}:${process.env.PATH}`,
        RELEASE_TAG: "v0.110.2",
        RELEASE_DRAFT: String(draft),
        RELEASE_ID: "123",
        GITHUB_REPOSITORY: "example/repository",
        RUNNER_TEMP: dir,
        FAIL_ATTEMPTS: String(failAttempts),
        INCOMPLETE: String(incomplete),
      },
    });
    let calls = [];
    try {
      calls = readFileSync(join(dir, "calls"), "utf8").trim().split("\n");
    } catch {}
    return { ...result, calls };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test("retries a failed asset without uploading successful files again", () => {
  const result = upload({ failAttempts: 2 });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.calls, [
    "downloads/one.dmg",
    "downloads/two.deb",
    "downloads/two.deb",
    "downloads/two.deb",
  ]);
});

test("stops publication after four failed attempts", () => {
  const result = upload({ failAttempts: 10 });
  assert.notEqual(result.status, 0);
  assert.equal(
    result.calls.filter((file) => file === "downloads/two.deb").length,
    4,
  );
  assert.match(result.stderr, /after four attempts/);
});

test("does not upload to a published immutable release", () => {
  const result = upload({ draft: false });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.calls, []);
});

test("refuses a complete list of names if an asset upload is unfinished", () => {
  const result = upload({ draft: false, incomplete: true });
  assert.notEqual(result.status, 0);
  assert.match(result.stdout, /two\.deb/);
});
