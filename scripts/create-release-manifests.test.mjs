import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";

import {
  createReleaseManifests,
  RELEASE_PLATFORMS,
  STAGING_RELEASE_PLATFORMS,
} from "./create-release-manifests.mjs";
import { STAGING_BASE_URL } from "./desktop-channel.mjs";
import { preparePublishedRelease } from "./prepare-published-release.mjs";

const EXPECTED_ARTIFACT_COUNT = RELEASE_PLATFORMS.reduce(
  (count, descriptor) =>
    count + descriptor.architectures.length * descriptor.formats.length,
  0,
);

function releaseFixture(version = "0.4.2", platforms = RELEASE_PLATFORMS) {
  const dist = mkdtempSync(path.join(tmpdir(), "tidebreak-release-"));
  for (const descriptor of platforms) {
    for (const arch of descriptor.architectures) {
      const directory = path.join(dist, descriptor.platform, arch);
      mkdirSync(directory, { recursive: true });
      const baseName = `Tidebreak_${version}_${arch}`;
      for (const format of descriptor.formats) {
        const file = path.join(directory, `${baseName}${format.extension}`);
        writeFileSync(file, `${format.format}-${descriptor.platform}-${arch}`);
        if (format.updater) {
          writeFileSync(
            `${file}.sig`,
            `signature-${descriptor.platform}-${arch}-${format.format}\n`,
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
  assert.deepEqual(Object.keys(latest.platforms), [
    "darwin-aarch64",
    "darwin-x86_64",
    "windows-x86_64",
    "linux-x86_64-appimage",
    "linux-x86_64-deb",
  ]);
  assert.match(
    latest.platforms["darwin-aarch64"].url,
    /releases\/v0\.4\.2\/macos\/universal\/Tidebreak_0\.4\.2_universal\.app\.tar\.gz$/,
  );
  assert.equal(
    latest.platforms["darwin-aarch64"].signature,
    "signature-macos-universal-app.tar.gz",
  );
  assert.deepEqual(
    latest.platforms["darwin-x86_64"],
    latest.platforms["darwin-aarch64"],
  );
  assert.match(
    latest.platforms["windows-x86_64"].url,
    /releases\/v0\.4\.2\/windows\/x86_64\/Tidebreak_0\.4\.2_x86_64-setup\.exe$/,
  );
  assert.equal(
    latest.platforms["windows-x86_64"].signature,
    "signature-windows-x86_64-nsis",
  );
  assert.match(
    latest.platforms["linux-x86_64-appimage"].url,
    /releases\/v0\.4\.2\/linux\/x86_64\/Tidebreak_0\.4\.2_x86_64\.AppImage$/,
  );
  assert.equal(
    latest.platforms["linux-x86_64-appimage"].signature,
    "signature-linux-x86_64-appimage",
  );
  assert.match(
    latest.platforms["linux-x86_64-deb"].url,
    /releases\/v0\.4\.2\/linux\/x86_64\/Tidebreak_0\.4\.2_x86_64\.deb$/,
  );
  assert.equal(
    latest.platforms["linux-x86_64-deb"].signature,
    "signature-linux-x86_64-deb",
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
    "universal",
    "Tidebreak_0.4.2_universal.app.tar.gz.sig",
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
  assert.throws(
    () =>
      createReleaseManifests({
        dist,
        ...RELEASE,
        baseUrl: STAGING_BASE_URL,
      }),
    /production base URL/,
  );
});

test("staging manifests stay under the staging prefix", () => {
  const version = "0.0.0-staging.12";
  const dist = releaseFixture(version, STAGING_RELEASE_PLATFORMS);
  const staging = {
    version,
    tag: "staging-12",
    sha: RELEASE.sha,
    publishedAt: RELEASE.publishedAt,
    baseUrl: STAGING_BASE_URL,
    channel: "staging",
  };
  const { latest, manifest } = createReleaseManifests({ dist, ...staging });

  assert.equal(manifest.version, version);
  assert.equal(manifest.tag, "staging-12");
  assert.match(
    latest.platforms["darwin-aarch64"].url,
    /\/tidebreak\/staging\/releases\/v0\.0\.0-staging\.12\//,
  );
  assert.deepEqual(Object.keys(latest.platforms), [
    "darwin-aarch64",
    "darwin-x86_64",
  ]);
  assert.throws(
    () =>
      createReleaseManifests({
        dist,
        ...staging,
        channel: "staging",
        baseUrl: RELEASE.baseUrl,
      }),
    /staging base URL/,
  );
  assert.throws(
    () =>
      createReleaseManifests({
        dist,
        ...RELEASE,
        channel: "staging",
        baseUrl: STAGING_BASE_URL,
      }),
    /invalid staging version/,
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
