import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import test from "node:test";

// The documents sandbox image preinstalls the document skills' pinned Python
// dependencies. That lockstep is by convention in three places — the SKILL.md
// manifests, the hash-checked requirements closure, and the Dockerfile that
// installs it — and silently drifts when a skill bumps a pin. These tests make
// the drift loud at PR time instead of leaving a stale image to be discovered
// by a background run.

const dockerfile = readFileSync(
  new URL("../crates/openwave-sandbox-agent/Dockerfile", import.meta.url),
  "utf8",
);
const requirements = readFileSync(
  new URL(
    "../crates/openwave-sandbox-agent/documents-requirements.txt",
    import.meta.url,
  ),
  "utf8",
);
const skillsDirectory = new URL("../skills/", import.meta.url);

function skillPins() {
  const pins = new Map();
  for (const name of readdirSync(skillsDirectory)) {
    let manifest;
    try {
      manifest = readFileSync(
        new URL(`${name}/SKILL.md`, skillsDirectory),
        "utf8",
      );
    } catch {
      continue;
    }
    // The manifest line is `deps: { python: [...], host: [...] }`; only the
    // python list carries pip pins — capture up to its own closing bracket so
    // a host list (or any later key) never bleeds into the pins.
    const deps = manifest.match(/^deps:\s*\{.*?python:\s*\[([^\]]*)\]/m);
    if (!deps) {
      continue;
    }
    for (const entry of deps[1].split(",")) {
      const pin = entry.trim().replace(/^"|"$/g, "");
      const [packageName, version] = pin.split("==");
      assert.ok(
        packageName && version,
        `${name}/SKILL.md declares an unpinned python dep: ${pin}`,
      );
      pins.set(packageName.toLowerCase(), version);
    }
  }
  return pins;
}

function requirementPins() {
  const pins = new Map();
  for (const match of requirements.matchAll(/^([A-Za-z0-9._-]+)==(\S+?)(?:\s|\\|$)/gm)) {
    pins.set(match[1].toLowerCase(), match[2]);
  }
  return pins;
}

test("every document-skill pin is preinstalled at the same version", () => {
  const skills = skillPins();
  const preinstalled = requirementPins();

  // All five document skills carry a manifest; a parser regression that finds
  // none would otherwise make this test pass vacuously.
  assert.ok(
    skills.size >= 5,
    `expected at least five SKILL.md python pins, found ${skills.size}`,
  );

  for (const [name, version] of skills) {
    assert.equal(
      preinstalled.get(name),
      version,
      `documents-requirements.txt is stale for ${name}==${version}; ` +
        "regenerate with scripts/generate-documents-requirements.py",
    );
  }

  // The bundled render helpers' own dependency rides the same closure.
  assert.ok(preinstalled.has("pypdfium2"));
});

test("the documents closure is hash-checked and installed that way", () => {
  // Every requirement entry carries at least one sha256 hash…
  const entries = requirements
    .replace(/\\\n/g, " ")
    .split("\n")
    .filter((line) => /^[A-Za-z0-9._-]+==/.test(line));
  assert.ok(entries.length >= 10);
  for (const entry of entries) {
    assert.match(
      entry,
      /--hash=sha256:[0-9a-f]{64}/,
      `unhashed requirement: ${entry.split(" ")[0]}`,
    );
  }

  // …and the Dockerfile refuses to install without verifying them.
  assert.match(dockerfile, /--require-hashes -r \/tmp\/documents-requirements\.txt/);
});

test("the image builds only from digest-pinned bases", () => {
  const stages = new Set();
  for (const line of dockerfile.split("\n")) {
    const from = line.match(/^FROM\s+(\S+)(?:\s+AS\s+(\S+))?/i);
    if (!from) {
      continue;
    }
    const [, base, stage] = from;
    if (!stages.has(base)) {
      assert.match(
        base,
        /@sha256:[0-9a-f]{64}$/,
        `Dockerfile base image is not digest-pinned: ${base}`,
      );
    }
    if (stage) {
      stages.add(stage);
    }
  }
  assert.ok(stages.has("documents"), "the documents stage must exist");
});

test("the publish workflow rebuilds when any image input changes", () => {
  const workflow = readFileSync(
    new URL(
      "../.github/workflows/publish-sandbox-image.yml",
      import.meta.url,
    ),
    "utf8",
  );

  // The main-push trigger publishes only when the resolve job sees an image
  // input change; these prefixes are that scope. The weekly schedule flushes
  // everything else (base-image patches) through regardless.
  const rebuildScopes = [
    "crates/openwave-sandbox-agent/*",
    "scripts/exec-documents/*",
    ".github/workflows/publish-sandbox-image.yml",
  ];
  for (const scope of rebuildScopes) {
    assert.ok(
      workflow.includes(scope),
      `publish workflow must rebuild on changes to ${scope}`,
    );
  }

  // Every in-repo path the final image stages copy from must fall inside a
  // rebuild scope, or a Dockerfile edit can silently stop republishing. The
  // builder stage's `COPY . .` is the compile context, deliberately covered
  // by the agent-crate scope plus the weekly rebuild rather than publishing
  // on every workspace commit.
  for (const match of dockerfile.matchAll(/^COPY\s+(?!--from=)(\S+)\s+\S+/gm)) {
    const source = match[1];
    if (source === ".") {
      continue;
    }
    assert.ok(
      rebuildScopes.some((scope) => source.startsWith(scope.replace(/\*$/, ""))),
      `Dockerfile input outside the publish rebuild scope: ${source}`,
    );
  }
});
