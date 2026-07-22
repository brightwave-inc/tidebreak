#!/usr/bin/env node

import { createHash } from "node:crypto";
import {
  existsSync,
  readFileSync,
  statSync,
  writeFileSync,
} from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

import { parseReleaseTag } from "./check-release-tag.mjs";

export const MACOS_ARCHITECTURES = ["aarch64", "x86_64"];

const ARTIFACT_FORMATS = [
  { extension: ".dmg", format: "dmg" },
  { extension: ".app.zip", format: "app.zip" },
  { extension: ".app.tar.gz", format: "app.tar.gz", updater: true },
];

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
        "usage: create-release-manifests.mjs --dist <path> --version <semver> --tag <tag> --sha <commit> --published-at <date> --base-url <url>",
      );
    }
    options.set(flag.slice(2), value);
  }
  return options;
}

function sha256(file) {
  return createHash("sha256").update(readFileSync(file)).digest("hex");
}

function publicUrl(baseUrl, version, filename) {
  const encodedPath = filename.split("/").map(encodeURIComponent).join("/");
  return `${baseUrl}/releases/v${version}/${encodedPath}`;
}

function requireFile(file) {
  if (!existsSync(file) || !statSync(file).isFile()) {
    throw new Error(`required release artifact is missing: ${file}`);
  }
}

export function createReleaseManifests({
  dist,
  version,
  tag,
  sha,
  publishedAt,
  baseUrl,
}) {
  const parsedTag = parseReleaseTag(tag);
  if (!parsedTag || parsedTag.version !== version) {
    throw new Error(`release tag ${tag} does not select version ${version}`);
  }
  if (!/^[0-9a-f]{40}$/.test(sha)) {
    throw new Error("release commit must be a full lowercase SHA-1");
  }
  if (Number.isNaN(Date.parse(publishedAt))) {
    throw new Error(`invalid release publication date: ${publishedAt}`);
  }

  const parsedBaseUrl = new URL(baseUrl);
  if (
    parsedBaseUrl.protocol !== "https:" ||
    parsedBaseUrl.hostname !== "downloads.brightwave.io"
  ) {
    throw new Error("release base URL must use https://downloads.brightwave.io");
  }
  const normalizedBaseUrl = baseUrl.replace(/\/+$/, "");
  const distPath = path.resolve(dist);
  const artifacts = [];
  const updaterArtifacts = new Map();

  for (const arch of MACOS_ARCHITECTURES) {
    const directory = path.join(distPath, "macos", arch);
    const baseName = `OpenWave_${version}_${arch}`;

    for (const descriptor of ARTIFACT_FORMATS) {
      const filename = `${baseName}${descriptor.extension}`;
      const file = path.join(directory, filename);
      requireFile(file);

      const digest = sha256(file);
      writeFileSync(`${file}.sha256`, `${digest}  ${filename}\n`);
      const relativeFilename = path.posix.join("macos", arch, filename);
      const checksumFilename = `${relativeFilename}.sha256`;
      const artifact = {
        platform: "macos",
        arch,
        format: descriptor.format,
        filename: relativeFilename,
        url: publicUrl(normalizedBaseUrl, version, relativeFilename),
        size: statSync(file).size,
        sha256: digest,
        checksum_filename: checksumFilename,
        checksum_url: publicUrl(normalizedBaseUrl, version, checksumFilename),
      };

      if (descriptor.updater) {
        const signatureFile = `${file}.sig`;
        requireFile(signatureFile);
        const signature = readFileSync(signatureFile, "utf8").trim();
        if (!signature) {
          throw new Error(`empty updater signature: ${signatureFile}`);
        }
        const signatureDigest = sha256(signatureFile);
        writeFileSync(
          `${signatureFile}.sha256`,
          `${signatureDigest}  ${path.basename(signatureFile)}\n`,
        );
        artifact.signature = signature;
        artifact.signature_filename = `${relativeFilename}.sig`;
        artifact.signature_url = publicUrl(
          normalizedBaseUrl,
          version,
          artifact.signature_filename,
        );
        artifact.signature_sha256 = signatureDigest;
        artifact.signature_checksum_url = publicUrl(
          normalizedBaseUrl,
          version,
          `${artifact.signature_filename}.sha256`,
        );
        updaterArtifacts.set(arch, artifact);
      }

      artifacts.push(artifact);
    }
  }

  const manifest = {
    schema_version: 1,
    version,
    tag,
    sha,
    published_at: publishedAt,
    artifacts,
  };
  const latest = {
    version,
    pub_date: publishedAt,
    platforms: Object.fromEntries(
      MACOS_ARCHITECTURES.map((arch) => {
        const artifact = updaterArtifacts.get(arch);
        return [
          `darwin-${arch}`,
          { signature: artifact.signature, url: artifact.url },
        ];
      }),
    ),
  };

  writeFileSync(
    path.join(distPath, "manifest.json"),
    `${JSON.stringify(manifest, null, 2)}\n`,
  );
  writeFileSync(
    path.join(distPath, "latest.json"),
    `${JSON.stringify(latest, null, 2)}\n`,
  );
  return { manifest, latest };
}

function main() {
  const options = parseOptions(process.argv.slice(2));
  const result = createReleaseManifests({
    dist: requiredOption(options, "dist"),
    version: requiredOption(options, "version"),
    tag: requiredOption(options, "tag"),
    sha: requiredOption(options, "sha"),
    publishedAt: requiredOption(options, "published-at"),
    baseUrl: requiredOption(options, "base-url"),
  });
  console.log(JSON.stringify(result.manifest, null, 2));
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  main();
}
