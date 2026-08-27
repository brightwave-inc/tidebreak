import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repositoryRoot = resolve(fileURLToPath(new URL("..", import.meta.url)));
const devScript = join(repositoryRoot, "scripts", "dev.sh");
function writeExecutable(path, source) {
  writeFileSync(path, source);
  chmodSync(path, 0o755);
}

function fakeTools() {
  const root = mkdtempSync(join(tmpdir(), "tidebreak-dev-test-"));
  const bin = join(root, "bin");
  const log = join(root, "calls.log");
  const preparedMarker = join(root, "sidecars-prepared");
  const target = join(root, "target");
  writeFileSync(log, "");
  mkdirSync(bin);

  writeExecutable(
    join(bin, "cargo"),
    `#!/usr/bin/env bash
set -euo pipefail
printf 'cargo|%s|%s\n' "$*" "\${CARGO_TARGET_DIR-}" >> "$FAKE_LOG"
if [[ "\${1-}" == "metadata" ]]; then
  printf '{"target_directory":"%s"}\n' "$FAKE_TARGET_DIR"
fi
`,
  );
  writeExecutable(
    join(bin, "pnpm"),
    `#!/usr/bin/env bash
set -euo pipefail
printf 'pnpm|%s\n' "$*" >> "$FAKE_LOG"
`,
  );
  writeExecutable(
    join(bin, "node"),
    `#!/usr/bin/env bash
set -euo pipefail
printf 'node|%s\n' "$*" >> "$FAKE_LOG"
if [[ "\${1-}" == "-e" ]]; then
  cat >/dev/null
  printf '%s' "$FAKE_TARGET_DIR"
else
  touch "$FAKE_PREPARED_MARKER"
fi
`,
  );
  writeExecutable(
    join(bin, "lsof"),
    `#!/usr/bin/env bash
set -euo pipefail
if [[ -n "\${FAKE_DEV_SERVER_PID-}" ]]; then
  printf '%s\n' "$FAKE_DEV_SERVER_PID"
  exit 0
fi
if [[ -n "\${FAKE_DEV_SERVER_PID_AFTER_PREP-}" && -e "$FAKE_PREPARED_MARKER" ]]; then
  printf '%s\n' "$FAKE_DEV_SERVER_PID_AFTER_PREP"
  exit 0
fi
exit 1
`,
  );

  return {
    cleanup: () => rmSync(root, { recursive: true, force: true }),
    env: {
      ...process.env,
      FAKE_LOG: log,
      FAKE_PREPARED_MARKER: preparedMarker,
      FAKE_TARGET_DIR: target,
      PATH: `${bin}:/usr/bin:/bin`,
    },
    log,
    target,
  };
}

test("scripts/dev.sh prepares sidecars before Tauri starts", () => {
  const tools = fakeTools();
  try {
    const result = spawnSync("bash", [devScript, "--no-watch"], {
      encoding: "utf8",
      env: tools.env,
    });
    assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);

    const calls = readFileSync(tools.log, "utf8").trim().split("\n");
    const install = calls.findIndex((call) => call.startsWith("pnpm|"));
    const prepare = calls.findIndex((call) =>
      call.includes("prepare-sidecar.mjs"),
    );
    const tauri = calls.findIndex((call) => call.startsWith("cargo|tauri dev"));

    assert.ok(install >= 0, calls.join("\n"));
    assert.ok(prepare > install, calls.join("\n"));
    assert.ok(tauri > prepare, calls.join("\n"));
    assert.match(calls[tauri], /--config .*beforeDevCommand.*pnpm dev/);
    assert.match(calls[tauri], /--no-watch/);
    assert.match(calls[tauri], new RegExp(`\\|${tools.target}$`));
  } finally {
    tools.cleanup();
  }
});

test("scripts/dev.sh stops before building when port 1420 is busy", () => {
  const tools = fakeTools();
  try {
    const result = spawnSync("bash", [devScript], {
      encoding: "utf8",
      env: { ...tools.env, FAKE_DEV_SERVER_PID: "4242" },
    });
    assert.equal(result.status, 1);
    assert.match(
      result.stderr,
      /dev port 1420 is already in use by PID\(s\): 4242/,
    );
    assert.doesNotMatch(readFileSync(tools.log, "utf8"), /pnpm|metadata/);
  } finally {
    tools.cleanup();
  }
});

test("scripts/dev.sh rechecks port 1420 after preparing sidecars", () => {
  const tools = fakeTools();
  try {
    const result = spawnSync("bash", [devScript], {
      encoding: "utf8",
      env: { ...tools.env, FAKE_DEV_SERVER_PID_AFTER_PREP: "4343" },
    });
    assert.equal(result.status, 1);
    assert.match(
      result.stderr,
      /dev port 1420 is already in use by PID\(s\): 4343/,
    );
    const calls = readFileSync(tools.log, "utf8");
    assert.match(calls, /prepare-sidecar\.mjs/);
    assert.doesNotMatch(calls, /cargo\|tauri dev/);
  } finally {
    tools.cleanup();
  }
});
