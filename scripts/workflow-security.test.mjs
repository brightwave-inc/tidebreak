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
  const cache = workflows["cache-macos.yml"];
  assert.ok(cache);
  assert.match(cache, /^on:\n  push:\n    branches: \[main\]/m);
  assert.match(cache, /^  workflow_dispatch:$/m);
  assert.doesNotMatch(cache, /^\s*pull_request(?:_target)?:/m);
  assert.match(cache, /^  cargo-downloads:$/m);
  assert.match(cache, /^    needs: cargo-downloads$/m);
  assert.match(cache, /cargo fetch --locked --target aarch64-apple-darwin/);
  assert.match(cache, /cargo fetch --locked --target x86_64-apple-darwin/);
  assert.match(cache, /cancel-in-progress: false/);
  assert.match(cache, /--no-bundle --ci/);
  assert.match(cache, /continue-on-error: true/);
  assert.doesNotMatch(cache, /^    environment:/m);
  assert.doesNotMatch(cache, /secrets\./);
  assert.doesNotMatch(cache, /APPLE_|TAURI_SIGNING|AWS_|DOWNLOADS_/);
  assert.doesNotMatch(cache, /actions\/upload-artifact/);

  const release = workflows["release.yml"];
  assert.doesNotMatch(release, /cache_warm_only/);
  assert.doesNotMatch(release, /^  warm-macos-cache:/m);
});

test("release caches restore only unsigned compiler outputs", () => {
  const release = workflows["release.yml"];
  const cache = workflows["cache-macos.yml"];
  const releaseBuildCache = release.match(
    /- name: Restore unsigned Rust build cache[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  const warmBuildCache = cache.match(
    /- name: Restore unsigned Rust build cache[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  const warmBuildCacheSave = cache.match(
    /- name: Save unsigned Rust build cache[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  assert.ok(releaseBuildCache);
  assert.ok(warmBuildCache);
  assert.ok(warmBuildCacheSave);

  assert.match(release, /SCCACHE_GHA_ENABLED: "true"/);
  assert.match(release, /SCCACHE_GHA_RW_MODE: READ_ONLY/);
  assert.match(release, /cache-targets: false/);
  assert.match(releaseBuildCache, /actions\/cache\/restore@[0-9a-f]{40}/);
  assert.doesNotMatch(release, /actions\/cache\/save/);
  assert.equal(release.match(/actions\/cache\//g)?.length, 1);
  assert.ok(
    release.indexOf("Restore unsigned Rust build cache") <
      release.indexOf("Validate production signing configuration"),
    "the build cache must be restored before production secrets are loaded",
  );

  assert.match(cache, /SCCACHE_GHA_ENABLED: "true"/);
  assert.match(cache, /SCCACHE_GHA_RW_MODE: READ_ONLY/);
  assert.match(cache, /cache-targets: false/);
  assert.match(warmBuildCache, /actions\/cache\/restore@[0-9a-f]{40}/);
  assert.match(cache, /actions\/cache\/save@[0-9a-f]{40}/);
  assert.ok(
    cache.indexOf("Save unsigned Rust build cache") <
      cache.indexOf("Require a successful cache-warm compilation"),
    "partial successful compiler outputs must be saved before a later failure is reported",
  );

  for (const cacheStep of [
    releaseBuildCache,
    warmBuildCache,
    warmBuildCacheSave,
  ]) {
    assert.match(cacheStep, /target\/release\/\.fingerprint/);
    assert.match(cacheStep, /target\/\$\{\{ matrix\.target \}\}\/release\/deps/);
    assert.doesNotMatch(cacheStep, /bundle|\.app|dmg|signature|keychain/i);
  }
});

test("the updater private key is isolated from compilation", () => {
  const release = workflows["release.yml"];
  const buildStep = release.match(
    /- name: Build, sign, and notarize the Tauri app[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  assert.ok(buildStep);
  assert.doesNotMatch(buildStep, /TAURI_SIGNING_PRIVATE_KEY/);
  assert.doesNotMatch(release, /createUpdaterArtifacts/);
  assert.match(release, /tauri signer sign "\$updater_path"/);
  assert.doesNotMatch(release, /cargo tauri signer sign/);
});

test("nested macOS native resources receive a timestamped Developer ID signature", () => {
  const release = workflows["release.yml"];
  assert.match(release, /security import "\$certificate_path"/);
  assert.match(release, /security find-identity -v -p codesigning/);
  assert.ok(
    release.indexOf("security import") <
      release.indexOf("Build, sign, and notarize the Tauri app"),
    "Developer ID certificate must be imported before beforeBundleCommand runs",
  );
  assert.match(release, /beforeBundleCommand/);
  assert.match(release, /bash scripts\/sign-macos-release-resources\.sh/);
  assert.match(
    release,
    /Contents\/Resources\/pdfium\/libpdfium\.dylib/,
  );
  assert.match(macosResourceSigner, /--sign "\$APPLE_SIGNING_IDENTITY"/);
  assert.match(macosResourceSigner, /--options runtime/);
  assert.match(macosResourceSigner, /--timestamp/);
  assert.match(release, /security delete-keychain/);
});

test("macOS disk images are explicitly notarized and stapled", () => {
  const release = workflows["release.yml"];
  const dmgNotarization = release.match(
    /- name: Notarize and staple the DMG[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  assert.ok(dmgNotarization);
  assert.match(dmgNotarization, /xcrun notarytool submit "\$dmg_path"/);
  assert.match(dmgNotarization, /--key "\$APPLE_API_KEY_PATH"/);
  assert.match(dmgNotarization, /xcrun stapler staple "\$dmg_path"/);
  assert.match(dmgNotarization, /xcrun stapler validate "\$dmg_path"/);
  assert.ok(
    release.indexOf("Build, sign, and notarize the Tauri app") <
      release.indexOf("Notarize and staple the DMG"),
  );
  assert.ok(
    release.indexOf("Notarize and staple the DMG") <
      release.indexOf("Verify and collect signed artifacts"),
  );
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
