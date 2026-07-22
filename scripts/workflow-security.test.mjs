import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import test from "node:test";

const workflowDirectory = new URL("../.github/workflows/", import.meta.url);
const workflows = Object.fromEntries(
  readdirSync(workflowDirectory)
    .filter((name) => name.endsWith(".yml") || name.endsWith(".yaml"))
    .map((name) => [name, readFileSync(new URL(name, workflowDirectory), "utf8")]),
);
const tauriConfig = JSON.parse(
  readFileSync(
    new URL("../crates/openwave-desktop/tauri.conf.json", import.meta.url),
    "utf8",
  ),
);
const macosResourceSigner = readFileSync(
  new URL("./sign-macos-release-resources.sh", import.meta.url),
  "utf8",
);

test("third-party workflow actions use immutable commit SHAs", () => {
  for (const [name, source] of Object.entries(workflows)) {
    for (const match of source.matchAll(/^\s*(?:-\s*)?uses:\s*([^\s#]+)/gm)) {
      const reference = match[1];
      if (reference.startsWith("./")) {
        continue;
      }
      assert.match(
        reference,
        /^[^@\s]+@[0-9a-f]{40}$/,
        `${name} has a mutable action reference: ${reference}`,
      );
    }
  }
});

test("workflow container images are pinned by digest", () => {
  for (const [name, source] of Object.entries(workflows)) {
    for (const match of source.matchAll(/^\s*image:\s*([^\s#]+)/gm)) {
      assert.match(
        match[1],
        /^[^@\s]+@sha256:[0-9a-f]{64}$/,
        `${name} has a mutable container image: ${match[1]}`,
      );
    }
  }

  assert.match(
    workflows["ci.yml"],
    /ghcr\.io\/gitleaks\/gitleaks@sha256:[0-9a-f]{64}/,
  );
  assert.doesNotMatch(workflows["ci.yml"], /gitleaks\/gitleaks:latest/);
});

test("production secrets remain isolated to the release workflow", () => {
  const secretConsumers = Object.entries(workflows)
    .filter(([, source]) => source.includes("secrets."))
    .map(([name]) => name);
  assert.deepEqual(secretConsumers, ["release.yml"]);

  const release = workflows["release.yml"];
  assert.match(release, /^on:\n  release:\n    types: \[published\]/m);
  assert.match(release, /^  workflow_dispatch:\n/m);
  assert.doesNotMatch(release, /^\s*pull_request(?:_target)?:/m);
  assert.match(release, /^permissions:\n  contents: read$/m);
});

test("release builds use the trusted shared main cache scope", () => {
  const release = workflows["release.yml"];
  assert.match(release, /gh workflow run release\.yml/);
  assert.match(release, /--ref main/);
  assert.match(release, /actions: write/);
  assert.match(
    release,
    /github\.event_name == 'workflow_dispatch' && github\.ref == 'refs\/heads\/main'/,
  );
  assert.match(
    release,
    /repos\/\$GITHUB_REPOSITORY\/releases\/tags\/\$RELEASE_TAG/,
  );
  assert.match(release, /ref: \$\{\{ needs\.validate\.outputs\.sha \}\}/);
});

test("cache warming cannot access production credentials or publish", () => {
  const release = workflows["release.yml"];
  const warmJob = release.match(
    /^  warm-macos-cache:[\s\S]*?(?=^  build-macos:)/m,
  )?.[0];
  assert.ok(warmJob);
  assert.match(warmJob, /--no-bundle --ci/);
  assert.doesNotMatch(warmJob, /^    environment:/m);
  assert.doesNotMatch(warmJob, /secrets\./);
  assert.doesNotMatch(warmJob, /APPLE_|TAURI_SIGNING|AWS_|DOWNLOADS_/);
});

test("release caches exclude signed artifacts and target directories", () => {
  const release = workflows["release.yml"];
  assert.match(release, /SCCACHE_GHA_ENABLED: "true"/);
  assert.match(release, /cache-targets: false/);
  assert.doesNotMatch(release, /actions\/cache/);
});

test("the updater private key is isolated from compilation", () => {
  const release = workflows["release.yml"];
  const buildStep = release.match(
    /- name: Build, sign, and notarize the Tauri app[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  assert.ok(buildStep);
  assert.doesNotMatch(buildStep, /TAURI_SIGNING_PRIVATE_KEY/);
  assert.doesNotMatch(release, /createUpdaterArtifacts/);
});

test("nested macOS native resources receive a timestamped Developer ID signature", () => {
  const release = workflows["release.yml"];
  assert.match(release, /beforeBundleCommand/);
  assert.match(release, /bash scripts\/sign-macos-release-resources\.sh/);
  assert.match(
    release,
    /Contents\/Resources\/pdfium\/libpdfium\.dylib/,
  );
  assert.match(macosResourceSigner, /--sign "\$APPLE_SIGNING_IDENTITY"/);
  assert.match(macosResourceSigner, /--options runtime/);
  assert.match(macosResourceSigner, /--timestamp/);
});

test("the packaged updater trusts the production signing key and endpoint", () => {
  const updater = tauriConfig.plugins?.updater;
  assert.ok(updater, "plugins.updater must exist when updater artifacts are built");
  assert.match(
    Buffer.from(updater.pubkey, "base64").toString("utf8"),
    /minisign public key/,
  );
  assert.deepEqual(updater.endpoints, [
    "https://downloads.brightwave.io/openwave/latest.json",
  ]);
});
