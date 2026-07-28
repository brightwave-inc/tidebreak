import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";

import {
  createReleaseManifests,
  MACOS_ARCHITECTURES,
} from "./create-release-manifests.mjs";
import { preparePublishedRelease } from "./prepare-published-release.mjs";

function releaseFixture() {
  const dist = mkdtempSync(path.join(tmpdir(), "openwave-release-"));
  for (const arch of MACOS_ARCHITECTURES) {
    const directory = path.join(dist, "macos", arch);
    mkdirSync(directory, { recursive: true });
    const baseName = `OpenWave_0.4.2_${arch}`;
    writeFileSync(path.join(directory, `${baseName}.dmg`), `dmg-${arch}`);
    writeFileSync(path.join(directory, `${baseName}.app.zip`), `zip-${arch}`);
    writeFileSync(
      path.join(directory, `${baseName}.app.tar.gz`),
      `updater-${arch}`,
    );
    writeFileSync(
      path.join(directory, `${baseName}.app.tar.gz.sig`),
      `signature-${arch}\n`,
    );
  }
  return dist;
}

const RELEASE = {
  version: "0.4.2",
  tag: "v0.4.2",
  sha: "0123456789abcdef0123456789abcdef01234567",
  publishedAt: "2026-07-22T16:00:00Z",
  baseUrl: "https://downloads.brightwave.io/openwave",
};

test("creates a complete manifest and Tauri updater document", () => {
  const dist = releaseFixture();
  const { manifest, latest } = createReleaseManifests({ dist, ...RELEASE });

  assert.equal(manifest.artifacts.length, MACOS_ARCHITECTURES.length * 3);
  assert.deepEqual(Object.keys(latest.platforms), ["darwin-aarch64"]);
  assert.match(
    latest.platforms["darwin-aarch64"].url,
    /releases\/v0\.4\.2\/macos\/aarch64\/OpenWave_0\.4\.2_aarch64\.app\.tar\.gz$/,
  );
  assert.equal(
    latest.platforms["darwin-aarch64"].signature,
    "signature-aarch64",
  );

  const diskManifest = JSON.parse(
    readFileSync(path.join(dist, "manifest.json"), "utf8"),
  );
  assert.deepEqual(diskManifest, manifest);
  for (const artifact of manifest.artifacts) {
    assert.equal(artifact.sha256.length, 64);
    assert.match(artifact.url, /^https:\/\/downloads\.brightwave\.io\/openwave\//);
    assert.match(
      readFileSync(path.join(dist, artifact.filename) + ".sha256", "utf8"),
      new RegExp(`^${artifact.sha256}  `),
    );
  }
});

test("fails closed when an architecture is incomplete", () => {
  const dist = releaseFixture();
  const missing = path.join(
    dist,
    "macos",
    "aarch64",
    "OpenWave_0.4.2_aarch64.app.tar.gz.sig",
  );
  writeFileSync(missing, "");

  assert.throws(
    () => createReleaseManifests({ dist, ...RELEASE }),
    /empty updater signature/,
  );
});

test("rejects mismatched tags and non-production hosts", () => {
  const dist = releaseFixture();
  assert.throws(
    () => createReleaseManifests({ dist, ...RELEASE, tag: "v0.4.3" }),
    /does not select version/,
  );
  assert.throws(
    () =>
      createReleaseManifests({
        dist,
        ...RELEASE,
        baseUrl: "https://example.com/openwave",
      }),
    /downloads\.brightwave\.io/,
  );
});

test("recreates latest metadata from an authoritative published manifest", () => {
  const dist = releaseFixture();
  const created = createReleaseManifests({ dist, ...RELEASE });
  const latestPath = path.join(dist, "resumed-latest.json");
  const resumed = preparePublishedRelease({
    manifestPath: path.join(dist, "manifest.json"),
    latestPath,
    ...RELEASE,
  });

  assert.deepEqual(resumed.latest, created.latest);
  assert.deepEqual(
    JSON.parse(readFileSync(latestPath, "utf8")),
    created.latest,
  );
});

test("rejects a published manifest that points outside its immutable prefix", () => {
  const dist = releaseFixture();
  createReleaseManifests({ dist, ...RELEASE });
  const manifestPath = path.join(dist, "manifest.json");
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  manifest.artifacts[0].url = "https://example.com/OpenWave.dmg";
  writeFileSync(manifestPath, `${JSON.stringify(manifest)}\n`);

  assert.throws(
    () =>
      preparePublishedRelease({
        manifestPath,
        latestPath: path.join(dist, "latest-resumed.json"),
        ...RELEASE,
      }),
    /unexpected artifact URL/,
  );
});
