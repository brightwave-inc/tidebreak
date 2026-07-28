import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import test from "node:test";

const workflowDirectory = new URL("../.github/workflows/", import.meta.url);
const workflows = Object.fromEntries(
  readdirSync(workflowDirectory)
    .filter((name) => name.endsWith(".yml") || name.endsWith(".yaml"))
    .map((name) => [name, readFileSync(new URL(name, workflowDirectory), "utf8")]),
);
const releaseDrafterConfig = readFileSync(
  new URL("../.github/release-drafter.yml", import.meta.url),
  "utf8",
);
const tauriConfig = JSON.parse(
  readFileSync(
    new URL("../crates/openwave-desktop/tauri.conf.json", import.meta.url),
    "utf8",
  ),
);
const desktopCargo = readFileSync(
  new URL("../crates/openwave-desktop/Cargo.toml", import.meta.url),
  "utf8",
);
const desktopHost = readFileSync(
  new URL("../crates/openwave-desktop/src/lib.rs", import.meta.url),
  "utf8",
);
const desktopUpdater = readFileSync(
  new URL("../crates/openwave-desktop/src/updater.rs", import.meta.url),
  "utf8",
);
const macosResourceSigner = readFileSync(
  new URL("./sign-macos-release-resources.sh", import.meta.url),
  "utf8",
);

function workflowJob(source, name) {
  const marker = `  ${name}:\n`;
  const start = source.indexOf(marker);
  assert.notEqual(start, -1, `missing workflow job: ${name}`);
  const remainder = source.slice(start + marker.length);
  const next = remainder.search(/^  [a-zA-Z0-9_-]+:\n/m);
  const end =
    next === -1 ? source.length : start + marker.length + next;
  return source.slice(start, end);
}

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

test("release-drafter retains a stable draft tag after formatting", () => {
  assert.match(releaseDrafterConfig, /tag-template: "v\$RESOLVED_VERSION"/);
  assert.match(releaseDrafterConfig, /^tag-prefix: "v"$/m);

  const draftJob = workflowJob(workflows["release-draft.yml"], "draft");
  assert.match(draftJob, /id: release_drafter/);
  assert.match(
    draftJob,
    /RELEASE_TAG: v\$\{\{ steps\.release_drafter\.outputs\.resolved_version \}\}/,
  );
  assert.match(
    draftJob,
    /\{tag_name: \$tag, body: \$body\}/,
  );
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

test("Rust CI requires the same PostgreSQL lane on pull requests and main", () => {
  const ci = workflows["ci.yml"];
  const postgres = workflowJob(ci, "postgres");
  const testJob = workflowJob(ci, "test");
  const aggregate = workflowJob(ci, "rust");

  assert.match(
    ci,
    /^on:\n  push:\n    branches: \[main\]\n  pull_request:/m,
  );
  assert.doesNotMatch(ci, /^  build:$/m);
  assert.match(postgres, /if:.*needs\.changes\.outputs\.rust == 'true'/);
  assert.doesNotMatch(postgres, /github\.event_name != 'pull_request'/);
  assert.match(postgres, /OPENWAVE_REQUIRE_POSTGRES_TEST: "true"/);
  assert.match(aggregate, /^\s+postgres,$/m);
  assert.match(aggregate, /test "\$POSTGRES_RESULT" = success/);
  assert.match(
    aggregate,
    /pull_request\) test "\$PR_TITLE_RESULT" = success ;;/,
  );
  assert.match(aggregate, /\*\) test "\$PR_TITLE_RESULT" = skipped ;;/);

  for (const job of [
    workflowJob(ci, "lint"),
    workflowJob(ci, "desktop"),
    testJob,
    postgres,
  ]) {
    assert.match(
      job,
      /shared-key: cargo-registry-v3-\$\{\{ hashFiles\('Cargo\.lock'\) \}\}/,
    );
    assert.match(job, /add-rust-environment-hash-key: "false"/);
    assert.match(job, /cache-targets: false/);
  }
  assert.match(testJob, /save-if: \$\{\{ github\.ref == 'refs\/heads\/main' \}\}/);
  assert.match(testJob, /cache-on-failure: true/);
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
  const dispatchJob = workflowJob(release, "dispatch");
  assert.match(release, /gh workflow run release\.yml/);
  assert.match(release, /--ref main/);
  assert.match(release, /actions: write/);
  assert.match(dispatchJob, /contents: read/);
  assert.match(
    dispatchJob,
    /node scripts\/check-release-tag\.mjs "\$RELEASE_TAG"/,
  );
  assert.ok(
    dispatchJob.indexOf("Reject an invalid published release tag") <
      dispatchJob.indexOf("gh workflow run release.yml"),
    "published release tags must be validated before dispatching a production build",
  );
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

  for (const workflow of [cache, release]) {
    const downloadCaches = [
      ...workflow.matchAll(
        /- name: Cache Cargo downloads[\s\S]*?(?=\n\s+- (?:name:|uses:))/g,
      ),
    ].map((match) => match[0]);
    assert.ok(downloadCaches.length > 0);
    for (const step of downloadCaches) {
      assert.match(
        step,
        /shared-key: macos-release-cargo-registry-v2-\$\{\{ hashFiles\('Cargo\.lock'\) \}\}/,
      );
      assert.match(step, /add-rust-environment-hash-key: "false"/);
      assert.match(step, /cache-targets: false/);
    }
  }
});

test("release caches restore only credential-free compiler products", () => {
  const release = workflows["release.yml"];
  const cache = workflows["cache-macos.yml"];
  const prepareJob = workflowJob(release, "prepare_macos");
  const signedBuildJob = workflowJob(release, "build_macos");
  const releasePrepareCache = prepareJob.match(
    /- name: Restore unsigned Rust build cache[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  const releasePrepareCacheSave = prepareJob.match(
    /- name: Save release-specific unsigned Rust build cache[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  const releaseBuildCache = signedBuildJob.match(
    /- name: Restore unsigned Rust build cache[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  const warmBuildCache = cache.match(
    /- name: Restore unsigned Rust build cache[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  const warmBuildCacheSave = cache.match(
    /- name: Save unsigned Rust build cache[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  assert.ok(releasePrepareCache);
  assert.ok(releasePrepareCacheSave);
  assert.ok(releaseBuildCache);
  assert.ok(warmBuildCache);
  assert.ok(warmBuildCacheSave);

  assert.match(prepareJob, /SCCACHE_GHA_ENABLED: "true"/);
  assert.match(prepareJob, /SCCACHE_GHA_RW_MODE: READ_ONLY/);
  assert.match(prepareJob, /cache-targets: false/);
  assert.match(prepareJob, /--no-bundle --ci/);
  assert.match(prepareJob, /continue-on-error: true/);
  assert.match(releasePrepareCache, /actions\/cache\/restore@[0-9a-f]{40}/);
  assert.match(releasePrepareCacheSave, /actions\/cache\/save@[0-9a-f]{40}/);
  assert.doesNotMatch(prepareJob, /^    environment:/m);
  assert.doesNotMatch(prepareJob, /secrets\./);
  assert.doesNotMatch(
    prepareJob,
    /APPLE_|TAURI_SIGNING|AWS_|DOWNLOADS_|actions\/upload-artifact/,
  );
  assert.ok(
    prepareJob.indexOf("Save release-specific unsigned Rust build cache") <
      prepareJob.indexOf("Require a successful unsigned compilation"),
    "release compiler outputs must be saved before a failed compile is reported",
  );

  assert.match(signedBuildJob, /SCCACHE_GHA_ENABLED: "true"/);
  assert.match(signedBuildJob, /SCCACHE_GHA_RW_MODE: READ_ONLY/);
  assert.match(signedBuildJob, /cache-targets: false/);
  assert.match(releaseBuildCache, /actions\/cache\/restore@[0-9a-f]{40}/);
  assert.doesNotMatch(signedBuildJob, /actions\/cache\/save/);
  assert.ok(
    signedBuildJob.indexOf("Restore unsigned Rust build cache") <
      signedBuildJob.indexOf("Validate production signing configuration"),
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
    releasePrepareCache,
    releasePrepareCacheSave,
    releaseBuildCache,
    warmBuildCache,
    warmBuildCacheSave,
  ]) {
    assert.match(cacheStep, /target\/release\/\.fingerprint/);
    assert.match(cacheStep, /target\/\$\{\{ matrix\.target \}\}\/release\/deps/);
    assert.match(
      cacheStep,
      /target\/\$\{\{ matrix\.target \}\}\/release\/openwave-desktop/,
    );
    assert.match(
      cacheStep,
      /target\/\$\{\{ matrix\.target \}\}\/release\/openwave-host-broker/,
    );
    assert.match(
      cacheStep,
      /crates\/openwave-desktop\/resources\/pdfium\/libpdfium\.dylib/,
    );
    assert.doesNotMatch(cacheStep, /bundle|\.app|dmg|signature|keychain/i);
  }

  for (const restoreStep of [
    releasePrepareCache,
    releaseBuildCache,
    warmBuildCache,
  ]) {
    assert.match(
      restoreStep,
      /macos-release-target-v2-\$\{\{ matrix\.arch \}\}-/,
      "unsigned product caches should be preferred when available",
    );
    assert.match(
      restoreStep,
      /macos-release-target-v1-\$\{\{ matrix\.arch \}\}-/,
      "unsigned compiler-only caches should remain a migration fallback",
    );
  }
});

test("an existing immutable release resumes without rebuilding or overwriting", () => {
  const release = workflows["release.yml"];
  const inspectJob = workflowJob(release, "inspect_hosted");
  const prepareJob = workflowJob(release, "prepare_macos");
  const signedBuildJob = workflowJob(release, "build_macos");
  const publishJob = workflowJob(release, "publish");

  assert.match(inspectJob, /id-token: write/);
  assert.match(inspectJob, /ref: \$\{\{ github\.sha \}\}/);
  assert.match(inspectJob, /prepare-published-release\.mjs/);
  assert.match(inspectJob, /Validated the complete immutable release/);
  assert.match(prepareJob, /needs\.inspect_hosted\.outputs\.exists != 'true'/);
  assert.match(
    signedBuildJob,
    /needs\.inspect_hosted\.outputs\.exists != 'true'/,
  );
  assert.match(
    publishJob,
    /needs\.inspect_hosted\.outputs\.exists == 'true'/,
  );
  assert.match(publishJob, /Resume from the hosted immutable release/);
  assert.match(publishJob, /ref: \$\{\{ github\.sha \}\}/);

  const immutableUpload = publishJob.match(
    /- name: Upload immutable release files[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  assert.ok(immutableUpload);
  assert.match(
    immutableUpload,
    /if: \$\{\{ needs\.inspect_hosted\.outputs\.exists != 'true' \}\}/,
  );
});

test("GitHub release downloads are copied from the hosted release", () => {
  const release = workflows["release.yml"];
  const attachJob = workflowJob(release, "attach_downloads");

  assert.match(attachJob, /needs: \[validate, publish\]/);
  assert.match(attachJob, /contents: write/);
  assert.doesNotMatch(attachJob, /^    environment:/m);
  assert.doesNotMatch(attachJob, /secrets\./);
  assert.doesNotMatch(attachJob, /APPLE_|TAURI_SIGNING|AWS_|DOWNLOADS_S3/);

  // Assets must be the CDN's own bytes, verified against the immutable
  // manifest, rather than a second copy built alongside the hosted release.
  assert.match(attachJob, /releases\/v\$OPENWAVE_VERSION\/manifest\.json/);
  assert.match(attachJob, /sha256sum --check --strict/);
  assert.match(attachJob, /OpenWave-macos-apple-silicon\.dmg/);
  assert.match(attachJob, /gh release upload "\$RELEASE_TAG"/);

  assert.match(
    readFileSync(new URL("../README.md", import.meta.url), "utf8"),
    /releases\/latest\/download\/OpenWave-macos-apple-silicon\.dmg/,
    "the README download link must match the published asset name",
  );
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

test("the packaged desktop activates the signed updater feed", () => {
  assert.match(desktopCargo, /tauri-plugin-updater = "=[^"]+"/);
  assert.match(
    desktopHost,
    /\.plugin\(tauri_plugin_updater::Builder::new\(\)\.build\(\)\)/,
  );
  assert.match(desktopHost, /\.manage\(updater::UpdateManager::default\(\)\)/);
  assert.match(desktopHost, /updater::spawn_update_loop\(handle\.clone\(\)\)/);
  assert.match(desktopHost, /updater::desktop_update_state/);
  assert.match(desktopHost, /updater::check_for_update/);
  assert.match(desktopHost, /updater::restart_for_update/);
  assert.match(desktopUpdater, /updater\.check\(\)\.await/);
  assert.match(desktopUpdater, /update\.download\(/);
  assert.doesNotMatch(desktopUpdater, /download_and_install/);
  assert.match(
    desktopUpdater,
    /state::<HostAccess>\(\)\.shutdown\(\)\.await[\s\S]*staged\.update\.install\(&staged\.bytes\)/,
  );
  assert.match(desktopUpdater, /app\.restart\(\)/);
  assert.match(
    desktopUpdater,
    /cfg!\(all\(not\(debug_assertions\), target_os = "macos"\)\)/,
  );
  assert.match(
    desktopUpdater,
    /const UPDATE_CHECK_STARTUP_DELAY: Duration = Duration::from_secs\(15\)/,
  );
  assert.match(
    desktopUpdater,
    /const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs\(5 \* 60\)/,
  );
});
