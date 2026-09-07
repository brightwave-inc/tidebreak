import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const root = new URL("../", import.meta.url);
const dockerfile = readFileSync(new URL("deploy/self-host/Dockerfile", root), "utf8");
const wrapper = readFileSync(new URL("deploy/supervised-agent/entrypoint.sh", root), "utf8");
const workflow = readFileSync(new URL(".github/workflows/publish-server-image.yml", root), "utf8");
const pins = readFileSync(new URL("crates/tidebreak-harness/src/pin.rs", root), "utf8");
const claudeVersion = pins.match(/kind: HarnessKind::ClaudeCode,\s*version: "([^"]+)"/)[1];

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
  const valid = runWrapper();
  assert.equal(valid.status, 0, valid.stderr);
  assert.equal(valid.stdout.trim(), "agent-started");
  for (const override of [
    { MODEL_GATEWAY_SANDBOX_ID: "" },
    { MODEL_GATEWAY_SANDBOX_SUPERVISOR_ENDPOINT: "" },
    { MODEL_GATEWAY_SANDBOX_GATEWAY_URL: "" },
    { MODEL_GATEWAY_SANDBOX_PLACEHOLDER_TOKEN: "private-fixture" },
    { GH_TOKEN: "private-fixture" },
    { TIDEBREAK_AGENT_ENGINE: "codex" },
  ]) {
    const refused = runWrapper(override);
    assert.notEqual(refused.status, 0, JSON.stringify(override));
    assert.equal(refused.stdout, "");
    assert.doesNotMatch(refused.stderr, /private-fixture/);
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
    'test "$(id -u)" = 65532; test -x /usr/local/bin/sandbox-agent; test -x /usr/local/bin/tidebreak-supervised-agent; test ! -e /usr/local/bin/tidebreak; test ! -e /opt/tidebreak/ui; git --version; gh --version; node --version; npm --version; npx --version; rustc --version; cargo --version; python3 --version; claude --version',
  ], { encoding: "utf8", timeout: 60000 });
  assert.equal(inspect.status, 0, inspect.stderr);
  assert.match(inspect.stdout, new RegExp(claudeVersion.replaceAll(".", "\\.") + " "));
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
