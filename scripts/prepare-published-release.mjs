#!/usr/bin/env node

import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

import {
  createLatestDocument,
  RELEASE_PLATFORMS,
} from "./create-release-manifests.mjs";

function requiredOption(options, name) {
  const value = options.get(name);
  if (!value) throw new Error(`missing required option --${name}`);
  return value;
}

function parseOptions(args) {
  const options = new Map();
  for (let index = 0; index < args.length; index += 2) {
    const flag = args[index];
    const value = args[index + 1];
    if (!flag?.startsWith("--") || value === undefined) {
      throw new Error(
        "usage: prepare-published-release.mjs --manifest <path> --latest <path> --version <semver> --tag <tag> --sha <commit> --published-at <date> --base-url <url>",
      );
    }
    options.set(flag.slice(2), value);
  }
  return options;
}

function publicUrl(baseUrl, version, filename) {
  const encodedPath = filename.split("/").map(encodeURIComponent).join("/");
  return `${baseUrl}/releases/v${version}/${encodedPath}`;
}

function requireExact(value, expected, description) {
  if (value !== expected) {
    throw new Error(`published release has unexpected ${description}`);
  }
}

export function validatePublishedReleaseManifest({
  manifest,
  version,
  tag,
  sha,
  publishedAt,
  baseUrl,
}) {
  requireExact(manifest.schema_version, 1, "schema version");
  requireExact(manifest.version, version, "version");
  requireExact(manifest.tag, tag, "tag");
  requireExact(manifest.sha, sha, "commit");
  requireExact(manifest.published_at, publishedAt, "publication date");

  const normalizedBaseUrl = baseUrl.replace(/\/+$/, "");
  const expected = new Map();
  for (const platformDescriptor of RELEASE_PLATFORMS) {
    for (const arch of platformDescriptor.architectures) {
      for (const descriptor of platformDescriptor.formats) {
        const filename = path.posix.join(
          platformDescriptor.platform,
          arch,
          `OpenWave_${version}_${arch}${descriptor.extension}`,
        );
        expected.set(filename, {
          platform: platformDescriptor.platform,
          arch,
          ...descriptor,
        });
      }
    }
  }

  if (!Array.isArray(manifest.artifacts)) {
    throw new Error("published release artifacts must be an array");
  }
  if (manifest.artifacts.length !== expected.size) {
    throw new Error("published release has an unexpected artifact count");
  }

  const seen = new Set();
  for (const artifact of manifest.artifacts) {
    const descriptor = expected.get(artifact.filename);
    if (!descriptor || seen.has(artifact.filename)) {
      throw new Error("published release has an unexpected artifact filename");
    }
    seen.add(artifact.filename);

    requireExact(artifact.platform, descriptor.platform, "artifact platform");
    requireExact(artifact.arch, descriptor.arch, "artifact architecture");
    requireExact(artifact.format, descriptor.format, "artifact format");
    if (!Number.isSafeInteger(artifact.size) || artifact.size <= 0) {
      throw new Error("published release has an invalid artifact size");
    }
    if (!/^[0-9a-f]{64}$/.test(artifact.sha256)) {
      throw new Error("published release has an invalid artifact digest");
    }

    const artifactUrl = publicUrl(
      normalizedBaseUrl,
      version,
      artifact.filename,
    );
    requireExact(artifact.url, artifactUrl, "artifact URL");
    requireExact(
      artifact.checksum_filename,
      `${artifact.filename}.sha256`,
      "artifact checksum filename",
    );
    requireExact(
      artifact.checksum_url,
      `${artifactUrl}.sha256`,
      "artifact checksum URL",
    );

    if (descriptor.updater) {
      if (typeof artifact.signature !== "string" || !artifact.signature) {
        throw new Error("published release has an empty updater signature");
      }
      if (!/^[0-9a-f]{64}$/.test(artifact.signature_sha256)) {
        throw new Error("published release has an invalid signature digest");
      }
      requireExact(
        artifact.signature_filename,
        `${artifact.filename}.sig`,
        "signature filename",
      );
      requireExact(
        artifact.signature_url,
        `${artifactUrl}.sig`,
        "signature URL",
      );
      requireExact(
        artifact.signature_checksum_url,
        `${artifactUrl}.sig.sha256`,
        "signature checksum URL",
      );
    }
  }

  return manifest;
}

export function preparePublishedRelease({
  manifestPath,
  latestPath,
  version,
  tag,
  sha,
  publishedAt,
  baseUrl,
}) {
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  validatePublishedReleaseManifest({
    manifest,
    version,
    tag,
    sha,
    publishedAt,
    baseUrl,
  });
  const latest = createLatestDocument({
    version,
    publishedAt,
    artifacts: manifest.artifacts,
  });
  writeFileSync(latestPath, `${JSON.stringify(latest, null, 2)}\n`);
  return { manifest, latest };
}

function main() {
  const options = parseOptions(process.argv.slice(2));
  const result = preparePublishedRelease({
    manifestPath: requiredOption(options, "manifest"),
    latestPath: requiredOption(options, "latest"),
    version: requiredOption(options, "version"),
    tag: requiredOption(options, "tag"),
    sha: requiredOption(options, "sha"),
    publishedAt: requiredOption(options, "published-at"),
    baseUrl: requiredOption(options, "base-url"),
  });
  console.log(JSON.stringify(result.latest, null, 2));
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  main();
}
