import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { installWorkloadHarnesses, workloadHarnessPins } from "../deploy/supervised-agent/install-harness.mjs";

const root = new URL("../", import.meta.url);
const dockerfile = readFileSync(new URL("deploy/self-host/Dockerfile", root), "utf8");
const wrapper = readFileSync(new URL("deploy/supervised-agent/entrypoint.sh", root), "utf8");
const workflow = readFileSync(new URL(".github/workflows/publish-server-image.yml", root), "utf8");
const pins = readFileSync(new URL("crates/tidebreak-harness/src/pin.rs", root), "utf8");
const workloadPins = workloadHarnessPins(pins);
const claudeVersion = workloadPins.find((pin) => pin.kind === "ClaudeCode").version;
const codexVersion = workloadPins.find((pin) => pin.kind === "Codex").version;

function runWrapper(overrides = {}) {
  const directory = mkdtempSync(join(tmpdir(), "tidebreak-workload-"));
  try {
    const path = join(directory, "entrypoint.sh");
    // Keep the guard under test; replace only the final binary with a witness.
    writeFileSync(path, wrapper.replace(
      "exec /usr/local/bin/tidebreak-supervised-agent",
      "exec /bin/echo agent-started",
    ));
    return spawnSync("/bin/sh", [path], {
      encoding: "utf8",
      env: {
        PATH: process.env.PATH,
        HOME: directory,
        MODEL_GATEWAY_SANDBOX_ID: "fixture",
        MODEL_GATEWAY_SANDBOX_SUPERVISOR_ENDPOINT: "127.0.0.1:15002",
        MODEL_GATEWAY_SANDBOX_GATEWAY_URL: "https://gateway.example.test",
        MODEL_GATEWAY_SANDBOX_PLACEHOLDER_TOKEN: "mg-sandbox-placeholder",
        GH_TOKEN: "mg-sandbox-placeholder",
        ...overrides,
      },
    });
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

test("the workload guards accept only the public placeholder and supported engine", () => {
  for (const engine of [undefined, "claude_code", "codex"]) {
    const valid = runWrapper(engine ? { TIDEBREAK_AGENT_ENGINE: engine } : {});
    assert.equal(valid.status, 0, valid.stderr);
    assert.equal(valid.stdout.trim(), "agent-started");
  }
  for (const override of [
    { MODEL_GATEWAY_SANDBOX_ID: "" },
    { MODEL_GATEWAY_SANDBOX_SUPERVISOR_ENDPOINT: "" },
    { MODEL_GATEWAY_SANDBOX_GATEWAY_URL: "" },
    { MODEL_GATEWAY_SANDBOX_PLACEHOLDER_TOKEN: "private-fixture" },
    { GH_TOKEN: "private-fixture" },
    { TIDEBREAK_AGENT_ENGINE: "internal" },
    { TIDEBREAK_AGENT_ENGINE: "custom" },
    { TIDEBREAK_AGENT_ENGINE: "unknown" },
  ]) {
    const refused = runWrapper(override);
    assert.notEqual(refused.status, 0, JSON.stringify(override));
    assert.equal(refused.stdout, "");
    assert.doesNotMatch(refused.stderr, /private-fixture/);
  }
});

test("the workload installs both exact pins and checks each installed binary", () => {
  const calls = [];
  installWorkloadHarnesses(pins, "/managed/npm", (command, args) => {
    calls.push({ command, args });
    if (command === "/usr/local/bin/claude") return `${claudeVersion} (Claude Code)\n`;
    if (command === "/usr/local/bin/codex") return `codex-cli ${codexVersion}\n`;
    return "";
  });
  assert.equal(calls[0].command, "/managed/npm");
  assert.ok(calls[0].args.includes("--engine-strict"), "unsupported package engines must fail installation");
  assert.deepEqual(calls[0].args.slice(-2), [
    `@anthropic-ai/claude-code@${claudeVersion}`,
    `@openai/codex@${codexVersion}`,
  ]);
  assert.deepEqual(calls.slice(1), [
    { command: "/usr/local/bin/claude", args: ["--version"] },
    { command: "/usr/local/bin/codex", args: ["--version"] },
  ]);
});

test("invalid pins and mismatched binaries stop the workload build", () => {
  for (const source of [
    pins.replace("HarnessKind::Codex,", "HarnessKind::Unknown,"),
    pins.replace('package: "@openai/codex"', 'package: "other-package"'),
    pins + pins,
  ]) {
    let invoked = false;
    assert.throws(() => installWorkloadHarnesses(source, "/managed/npm", () => { invoked = true; }), /package pin is missing or malformed/);
    assert.equal(invoked, false);
  }
  for (const mismatchedEngine of ["claude", "codex"]) {
    assert.throws(() => installWorkloadHarnesses(pins, "/managed/npm", (command) => {
      if (command === `/usr/local/bin/${mismatchedEngine}`) return "0.0.0";
      if (command === "/usr/local/bin/claude") return `${claudeVersion} (Claude Code)`;
      if (command === "/usr/local/bin/codex") return `codex-cli ${codexVersion}`;
      return "";
    }), /installed .* version does not match/);
  }
});

test("the BYO image installs Gateway's fixed command and keeps the server as default", () => {
  const workload = dockerfile.split("FROM runtime-base AS supervised-agent\n")[1].split("FROM runtime-base AS server\n")[0];
  assert.match(workload, /COPY --from=build \/out\/tidebreak-supervised-agent \/usr\/local\/bin\/tidebreak-supervised-agent/);
  assert.match(workload, /COPY deploy\/supervised-agent\/entrypoint.sh \/usr\/local\/bin\/sandbox-agent/);
  assert.match(workload, /USER 65532:65532/);
  assert.match(workload, /ENTRYPOINT \["\/usr\/local\/bin\/sandbox-agent"\]/);
  assert.doesNotMatch(workload, /COPY .*\/out\/tidebreak\s|COPY --from=renderer/);
  assert.equal(dockerfile.match(/^FROM .+$/gm).at(-1), "FROM runtime-base AS server");
});

test("release publication builds both images and preserves older server backfills", () => {
  assert.match(workflow, /images='\[{"name":"server","target":""}\]'/);
  assert.match(workflow, /{"name":"supervised-agent","target":"supervised-agent"}/);
  assert.match(workflow, /git show "\$build_sha:deploy\/self-host\/Dockerfile"/);
  assert.match(workflow, /artifact: \$\{\{ fromJSON\(needs.resolve.outputs.images\) \}\}/);
  assert.match(workflow, /TIDEBREAK_SUPERVISED_AGENT_IMAGE:/);
  assert.match(workflow, /node --test scripts\/supervised-agent-image.test.mjs/);
});

test("the shared publication workflow has one job per stage and valid shell blocks", () => {
  for (const job of ["resolve", "build", "manifest"]) {
    assert.equal(workflow.match(new RegExp("^  " + job + ":$", "gm"))?.length, 1);
  }
  for (const block of workflow.matchAll(/^        run: \|\n((?:^          .*\n|^\n)+)/gm)) {
    const script = block[1].replace(/^          /gm, "").replace(/\$\{\{[^\n]*?\}\}/g, "fixture");
    const parsed = spawnSync("bash", ["-n"], { input: script, encoding: "utf8" });
    assert.equal(parsed.status, 0, parsed.stderr);
  }
});

const image = process.env.TIDEBREAK_SUPERVISED_AGENT_IMAGE;
test("the built workload starts as UID 65532 with pinned tools and no server", { skip: !image }, () => {
  const inspect = spawnSync("docker", ["run", "--rm", "--network=none", "--entrypoint", "/bin/sh", image, "-ec",
    'test "$(id -u)" = 65532; test -x /usr/local/bin/sandbox-agent; test -x /usr/local/bin/tidebreak-supervised-agent; test ! -e /usr/local/bin/tidebreak; test ! -e /opt/tidebreak/ui; git --version; gh --version; node --version; npm --version; npx --version; rustc --version; cargo --version; python3 --version; claude --version; codex --version',
  ], { encoding: "utf8", timeout: 60000 });
  assert.equal(inspect.status, 0, inspect.stderr);
  assert.match(inspect.stdout, new RegExp(claudeVersion.replaceAll(".", "\\.") + " "));
  assert.match(inspect.stdout, new RegExp("codex-cli " + codexVersion.replaceAll(".", "\\.")));
  const committed = spawnSync("docker", ["run", "--rm", "--network=none", "--entrypoint", "/bin/sh",
    "--env", "GIT_AUTHOR_NAME=fixture[bot]", "--env", "GIT_COMMITTER_NAME=fixture[bot]",
    "--env", "GIT_AUTHOR_EMAIL=8675309+fixture[bot]@users.noreply.github.com",
    "--env", "GIT_COMMITTER_EMAIL=8675309+fixture[bot]@users.noreply.github.com",
    image, "-ec", 'directory=$(mktemp -d); cd "$directory"; git init -q; touch change; git add change; git commit -qm "Verify sandbox authorship"; git show -s --format="%an <%ae> | %cn <%ce>"',
  ], { encoding: "utf8", timeout: 60000 });
  assert.equal(committed.status, 0, committed.stderr);
  assert.equal(committed.stdout.trim(), "fixture[bot] <8675309+fixture[bot]@users.noreply.github.com> | fixture[bot] <8675309+fixture[bot]@users.noreply.github.com>");
  const missingContract = spawnSync("docker", ["run", "--rm", "--network=none", image], { encoding: "utf8", timeout: 60000 });
  assert.notEqual(missingContract.status, 0);
  assert.match(missingContract.stderr, /Gateway must provide a sandbox identity/);
  const missingInput = spawnSync("docker", ["run", "--rm", "--network=none", "--entrypoint", "/usr/local/bin/tidebreak-supervised-agent", image], { encoding: "utf8", timeout: 60000 });
  assert.equal(missingInput.status, 64, missingInput.stderr);
  assert.match(missingInput.stderr, /MODEL_GATEWAY_SANDBOX_TASK/);
});

test("the built workload Node satisfies both installed harness requirements", { skip: !image }, () => {
  const probe = `
    const assert = require("node:assert/strict");
    const { realpathSync, readFileSync } = require("node:fs");
    const { execFileSync } = require("node:child_process");
    const { createRequire } = require("node:module");
    assert.equal(process.execPath, "/usr/local/bin/node", "PATH must select the workload Node runtime");
    for (const binary of ["npm", "npx"]) {
      const path = execFileSync("/bin/sh", ["-c", 'command -v "$1"', "probe", binary], { encoding: "utf8" }).trim();
      assert.equal(path, "/usr/local/bin/" + binary);
      const target = realpathSync(path);
      assert.equal(target, "/usr/local/lib/node_modules/npm/bin/" + binary + "-cli.js");
      assert(readFileSync(target, "utf8").startsWith("#!/usr/bin/env node\\n"), binary + " must use the workload Node through PATH");
    }
    const { satisfies } = createRequire(realpathSync("/usr/local/bin/npm"))("semver");
    for (const name of ${JSON.stringify(workloadPins.map((pin) => pin.package))}) {
      const manifest = require("/usr/local/lib/node_modules/" + name + "/package.json");
      const required = manifest.engines?.node;
      if (required) {
        assert(satisfies(process.version, required), name + " requires Node " + required + ", found " + process.version);
      }
      console.log(name + ": Node " + process.version + " satisfies " + (required ?? "any version"));
    }
  `;
  const supported = spawnSync("docker", ["run", "--rm", "--network=none", "--entrypoint", "/usr/bin/env", image, "node", "-e", probe], { encoding: "utf8", timeout: 60000 });
  assert.equal(supported.status, 0, supported.stderr);
  for (const pin of workloadPins) assert.ok(supported.stdout.includes(pin.package));
});
