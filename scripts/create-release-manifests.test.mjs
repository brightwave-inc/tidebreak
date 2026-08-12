import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";

import {
  createReleaseManifests,
  RELEASE_PLATFORMS,
} from "./create-release-manifests.mjs";
import { preparePublishedRelease } from "./prepare-published-release.mjs";

const EXPECTED_ARTIFACT_COUNT = RELEASE_PLATFORMS.reduce(
  (count, descriptor) =>
    count + descriptor.architectures.length * descriptor.formats.length,
  0,
);

function releaseFixture() {
  const dist = mkdtempSync(path.join(tmpdir(), "tidebreak-release-"));
  for (const descriptor of RELEASE_PLATFORMS) {
    for (const arch of descriptor.architectures) {
      const directory = path.join(dist, descriptor.platform, arch);
      mkdirSync(directory, { recursive: true });
      const baseName = `Tidebreak_0.4.2_${arch}`;
      for (const format of descriptor.formats) {
        const file = path.join(directory, `${baseName}${format.extension}`);
        writeFileSync(file, `${format.format}-${descriptor.platform}-${arch}`);
        if (format.updater) {
          writeFileSync(
            `${file}.sig`,
            `signature-${descriptor.platform}-${arch}\n`,
          );
        }
      }
    }
  }
  return dist;
}

const RELEASE = {
  version: "0.4.2",
  tag: "v0.4.2",
  sha: "0123456789abcdef0123456789abcdef01234567",
  publishedAt: "2026-07-22T16:00:00Z",
  baseUrl: "https://downloads.brightwave.io/tidebreak",
};

test("creates a complete manifest and Tauri updater document", () => {
  const dist = releaseFixture();
  const { manifest, latest } = createReleaseManifests({ dist, ...RELEASE });

  assert.equal(manifest.artifacts.length, EXPECTED_ARTIFACT_COUNT);
  assert.deepEqual(Object.keys(latest.platforms), ["darwin-aarch64"]);
  assert.match(
    latest.platforms["darwin-aarch64"].url,
    /releases\/v0\.4\.2\/macos\/aarch64\/Tidebreak_0\.4\.2_aarch64\.app\.tar\.gz$/,
  );
  assert.equal(
    latest.platforms["darwin-aarch64"].signature,
    "signature-macos-aarch64",
  );

  const diskManifest = JSON.parse(
    readFileSync(path.join(dist, "manifest.json"), "utf8"),
  );
  assert.deepEqual(diskManifest, manifest);
  for (const artifact of manifest.artifacts) {
    assert.equal(artifact.sha256.length, 64);
    assert.match(artifact.url, /^https:\/\/downloads\.brightwave\.io\/tidebreak\//);
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
    "Tidebreak_0.4.2_aarch64.app.tar.gz.sig",
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
        baseUrl: "https://example.com/tidebreak",
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
  manifest.artifacts[0].url = "https://example.com/Tidebreak.dmg";
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
