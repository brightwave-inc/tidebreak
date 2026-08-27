import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

// The self-host image ships the tools code mode spawns: the managed Node
// runtime it installs pinned harness packages with, git, and the GitHub CLI.
// The server verifies that Node runtime against one exact version and one
// exact artifact digest per platform, and resolves it from one exact path — so
// an image whose pin drifts from the Rust constant does not fall back to
// something workable, it reports every engine as not found. These tests make
// the drift loud at PR time instead of leaving it to an operator's first
// code-mode session.

const dockerfile = readFileSync(
  new URL("../deploy/self-host/Dockerfile", import.meta.url),
  "utf8",
);
const entrypoint = readFileSync(
  new URL("../deploy/self-host/entrypoint.sh", import.meta.url),
  "utf8",
);
const managedNode = readFileSync(
  new URL("../crates/tidebreak-managed-node/src/lib.rs", import.meta.url),
  "utf8",
);

/** The `RUN` block that mentions `marker`, with its line continuations intact. */
function runBlock(marker) {
  const lines = dockerfile.split("\n");
  const blocks = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (!lines[index].startsWith("RUN ")) {
      continue;
    }
    const block = [lines[index]];
    while (block.at(-1).endsWith("\\") && index + 1 < lines.length) {
      index += 1;
      block.push(lines[index]);
    }
    blocks.push(block.join("\n"));
  }
  const block = blocks.find((candidate) => candidate.includes(marker));
  assert.ok(block, `the Dockerfile must carry a RUN block installing ${marker}`);
  return block;
}

function pinnedNodeVersion() {
  const match = managedNode.match(/MANAGED_NODE_VERSION: &str = "([^"]+)"/);
  assert.ok(match, "tidebreak-managed-node must declare MANAGED_NODE_VERSION");
  return match[1];
}

function pinnedNodeDigest(architecture) {
  const match = managedNode.match(
    new RegExp(
      `target_os = "linux", target_arch = "${architecture}"[\\s\\S]*?artifact_sha256: "([0-9a-f]{64})"`,
    ),
  );
  assert.ok(match, `tidebreak-managed-node must pin the linux ${architecture} digest`);
  return match[1];
}

/** The `platform` and `sha256` a RUN block selects for a Debian architecture. */
function architectureBranch(block, debianArchitecture) {
  const branch = block.match(
    new RegExp(
      `${debianArchitecture}\\) platform=(\\S+); \\\\\\n\\s*sha256=([0-9a-f]{64})`,
    ),
  );
  assert.ok(branch, `no ${debianArchitecture} branch in this install step`);
  return { platform: branch[1], sha256: branch[2] };
}

test("the image installs the Node version the server verifies", () => {
  const version = pinnedNodeVersion();
  const escaped = version.replaceAll(".", "\\.");
  assert.match(
    runBlock("nodejs.org"),
    new RegExp(`^\\s*version=${escaped};`, "m"),
    `the Dockerfile must install Node ${version}`,
  );
  assert.match(
    entrypoint,
    new RegExp(`^node_version=${escaped}$`, "m"),
    `entrypoint.sh must publish Node ${version}`,
  );
});

test("each architecture takes the Node digest its platform pin names", () => {
  const block = runBlock("nodejs.org");
  for (const [architecture, debian, platform] of [
    ["x86_64", "amd64", "linux-x64"],
    ["aarch64", "arm64", "linux-arm64"],
  ]) {
    const branch = architectureBranch(block, debian);
    assert.equal(branch.platform, platform);
    assert.equal(
      branch.sha256,
      pinnedNodeDigest(architecture),
      `the ${debian} Node digest does not match the ${architecture} pin`,
    );
  }
});

test("the Node install marker carries the field names the server reads", () => {
  const block = runBlock("nodejs.org");
  // tidebreak-managed-node deserializes `version` and `artifactSha256`; a
  // marker with any other shape reads as no install at all.
  assert.match(block, /"version": "%s"/);
  assert.match(block, /"artifactSha256": "%s"/);
  // The digest is checked before a byte is unpacked, and the tree is what the
  // marker then vouches for.
  assert.match(block, /sha256sum --check --strict/);
});

test("git comes from the pinned Debian snapshot", () => {
  // Clone, worktree, checkpoint, commit, and push all spawn git, and the
  // snapshot pin is what keeps a rebuild of one commit reproducible.
  assert.match(
    dockerfile,
    /^\s+git=1:\S+ \\$/m,
    "the runtime stage must install git at an exact version",
  );
});

test("gh comes from the project's own release, digest-pinned", () => {
  // Debian bookworm carries gh 2.23.0, which has no `autoMergeRequest` JSON
  // field. Tidebreak asks for it alongside every other field in one request,
  // so that build fails the whole pull-request digest.
  assert.doesNotMatch(
    dockerfile,
    /^\s+gh=/m,
    "gh from apt is too old for the fields Tidebreak requests",
  );
  const block = runBlock("cli/cli/releases");
  assert.match(block, /^\s*version=\d+\.\d+\.\d+;/m);
  assert.match(block, /sha256sum --check --strict/);
  for (const [debian, platform] of [
    ["amd64", "linux_amd64"],
    ["arm64", "linux_arm64"],
  ]) {
    assert.equal(architectureBranch(block, debian).platform, platform);
  }
});
