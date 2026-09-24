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
const compose = readFileSync(
  new URL("../deploy/self-host/docker-compose.yml", import.meta.url),
  "utf8",
);
const caddyfile = readFileSync(
  new URL("../deploy/self-host/Caddyfile", import.meta.url),
  "utf8",
);
const checkPins = readFileSync(
  new URL("../deploy/self-host/check-pins.sh", import.meta.url),
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

// The compose stack runs third-party images beside the server. Each one is
// pinned by digest, so `docker compose pull` cannot swap in different bytes
// under the same tag; the server itself follows the release in .env.
test("every image the compose file runs is digest-pinned, except the release", () => {
  const images = [...compose.matchAll(/^\s+image: (.+)$/gm)].map((match) => match[1]);
  assert.ok(images.length >= 3, "postgres, server, and caddy");
  for (const image of images) {
    if (image.startsWith("ghcr.io/brightwave-inc/tidebreak-server:")) {
      assert.match(image, /:\$\{TIDEBREAK_VERSION:\?/);
      continue;
    }
    assert.match(image, /^[a-z0-9./-]+:[\w.-]+@sha256:[0-9a-f]{64}$/, `${image} is not digest-pinned`);
  }
});

test("Caddy is pinned to an exact release, and check-pins.sh verifies the pin", () => {
  const caddy = compose.match(/^\s+image: (caddy:\S+)$/m);
  assert.ok(caddy, "docker-compose.yml runs Caddy");
  assert.match(caddy[1], /^caddy:\d+\.\d+\.\d+-alpine@sha256:[0-9a-f]{64}$/);
  // check-pins.sh reads this file and asks Docker Hub's official repository
  // for the pinned index, so a typed digest fails in CI.
  assert.match(checkPins, /docker-compose\.yml/);
  assert.match(checkPins, /registry-1\.docker\.io\/v2\/library\/caddy\/manifests/);
});

/**
 * The Caddyfile's top-level blocks, keyed by the line that opens each one
 * (`{` for the global options), each as its directive lines with comments
 * and indentation removed.
 */
function caddyBlocks(text) {
  const blocks = new Map();
  let depth = 0;
  let current = null;
  for (const raw of text.split("\n")) {
    const line = raw.replace(/#.*$/, "").trim();
    if (!line) {
      continue;
    }
    if (depth === 0) {
      assert.ok(line.endsWith("{"), `unexpected top-level line: ${line}`);
      current = [];
      blocks.set(line, current);
    } else if (!(depth === 1 && line === "}")) {
      current.push(line);
    }
    depth += (line.match(/{/g) ?? []).length - (line.match(/}/g) ?? []).length;
  }
  assert.equal(depth, 0, "the Caddyfile's braces balance");
  return blocks;
}

test("the Caddyfile proxies to the server and keeps bearer tokens out of every log", () => {
  const blocks = caddyBlocks(caddyfile);
  const site = blocks.get("{$TIDEBREAK_DOMAIN} {");
  assert.ok(site, "the site is the configured domain");
  assert.ok(site.includes("reverse_proxy server:8080"));
  // No access log: a site's own `log` would use its own format, outside the
  // filter below.
  assert.deepEqual(
    site.filter((line) => /^log\b/.test(line)),
    [],
    "a site `log` directive would record bearer tokens",
  );

  // Caddy's default log still records the request headers of every error
  // it answers, such as a 502 while the server restarts. Tokens ride in
  // Authorization and, for a browser's WebSocket upgrade, in
  // Sec-WebSocket-Protocol, so the default log must delete both.
  const global = blocks.get("{");
  assert.ok(global, "the Caddyfile has a global options block");
  const log = global.indexOf("log default {");
  assert.notEqual(log, -1, "the global options configure the default log");
  const filter = global.indexOf("format filter {", log);
  assert.notEqual(filter, -1, "the default log uses a filter format");
  const closing = global.indexOf("}", filter);
  const fields = global.slice(filter + 1, closing);
  for (const header of ["Authorization", "Sec-Websocket-Protocol"]) {
    assert.ok(
      fields.includes(`request>headers>${header} delete`),
      `the default log must delete request>headers>${header}`,
    );
  }
  assert.match(compose, /- \.\/Caddyfile:\/etc\/caddy\/Caddyfile:ro\n/);
});
