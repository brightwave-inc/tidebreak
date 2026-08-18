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
import { desktopChannel } from "./desktop-channel.mjs";
import { parseStagingVersion, stagingTag } from "./staging-version.mjs";

// The platforms, architectures, and artifact formats a release ships. This is
// the single source of truth for what a release contains: it drives the
// manifest, the `latest.json` platform keys, and the immutable hosting prefix.
// macOS ships one universal artifact whose signed updater archive is advertised
// to both native architectures; see docs/releases.md.
//
const MACOS_PLATFORM = {
  platform: "macos",
  updaterPlatform: "darwin",
  architectures: ["universal"],
  updaterArchitectures: ["aarch64", "x86_64"],
  formats: [
    { extension: ".dmg", format: "dmg" },
    { extension: ".app.zip", format: "app.zip" },
    { extension: ".app.tar.gz", format: "app.tar.gz", updater: true },
  ],
};

const WINDOWS_PLATFORM = {
  platform: "windows",
  updaterPlatform: "windows",
  architectures: ["x86_64", "aarch64"],
  // Tauri v2 installs Windows updates from the NSIS installer itself, so the
  // updater signature covers the exact bytes users download.
  formats: [{ extension: "-setup.exe", format: "nsis", updater: true }],
};

const LINUX_PLATFORM = {
  platform: "linux",
  updaterPlatform: "linux",
  architectures: ["x86_64", "aarch64"],
  formats: [
    // Tauri selects Linux updates by the bundle format the running app was
    // installed from. Publish distinct targets so a Debian install can never
    // fall back to AppImage bytes (or vice versa).
    {
      extension: ".AppImage",
      format: "appimage",
      updater: true,
      updaterKeySuffix: "appimage",
    },
    {
      extension: ".deb",
      format: "deb",
      updater: true,
      updaterKeySuffix: "deb",
    },
  ],
};

export const RELEASE_PLATFORMS = [
  MACOS_PLATFORM,
  WINDOWS_PLATFORM,
  LINUX_PLATFORM,
];

// Staging remains the signed macOS channel described by decision 16. Production
// platform expansion must not make that independent workflow require packages
// it does not build.
export const STAGING_RELEASE_PLATFORMS = [MACOS_PLATFORM];

function releasePlatforms(channel) {
  return channel === "staging"
    ? STAGING_RELEASE_PLATFORMS
    : RELEASE_PLATFORMS;
}

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
        "usage: create-release-manifests.mjs --dist <path> --version <semver> --tag <tag> --sha <commit> --published-at <date> --base-url <url> [--channel production|staging]",
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

export function createLatestDocument({
  version,
  publishedAt,
  artifacts,
  platforms = RELEASE_PLATFORMS,
}) {
  const updaterArtifacts = new Map();
  const updaterKeys = [];
  for (const descriptor of platforms) {
    for (const updaterFormat of descriptor.formats.filter(
      (format) => format.updater,
    )) {
      const updaterArchitectures =
        descriptor.updaterArchitectures ?? descriptor.architectures;
      const keysForFormat = updaterArchitectures.map((arch) =>
        [
          descriptor.updaterPlatform,
          arch,
          updaterFormat.updaterKeySuffix,
        ]
          .filter(Boolean)
          .join("-"),
      );
      updaterKeys.push(...keysForFormat);

      for (const artifact of artifacts) {
        if (
          artifact.platform !== descriptor.platform ||
          artifact.format !== updaterFormat.format
        ) {
          continue;
        }
        if (
          !descriptor.architectures.includes(artifact.arch) ||
          typeof artifact.signature !== "string" ||
          !artifact.signature
        ) {
          throw new Error(
            `invalid ${descriptor.platform} updater artifact in release manifest`,
          );
        }
        const artifactArchitectures = descriptor.updaterArchitectures ?? [
          artifact.arch,
        ];
        for (const arch of artifactArchitectures) {
          const key = [
            descriptor.updaterPlatform,
            arch,
            updaterFormat.updaterKeySuffix,
          ]
            .filter(Boolean)
            .join("-");
          updaterArtifacts.set(key, artifact);
        }
      }
    }
  }

  return {
    version,
    pub_date: publishedAt,
    platforms: Object.fromEntries(
      updaterKeys.map((key) => {
        const artifact = updaterArtifacts.get(key);
        if (!artifact) {
          throw new Error(
            `missing ${key} updater artifact in release manifest`,
          );
        }
        return [key, { signature: artifact.signature, url: artifact.url }];
      }),
    ),
  };
}

function assertChannelVersion({ channelId, version, tag, baseUrl }) {
  const channel = desktopChannel(channelId);
  const parsedBaseUrl = new URL(baseUrl);
  if (
    parsedBaseUrl.protocol !== "https:" ||
    parsedBaseUrl.hostname !== "downloads.brightwave.io"
  ) {
    throw new Error("release base URL must use https://downloads.brightwave.io");
  }
  const normalizedBaseUrl = baseUrl.replace(/\/+$/, "");
  if (normalizedBaseUrl !== channel.baseUrl) {
    throw new Error(
      `${channelId} base URL must be ${channel.baseUrl}, not ${normalizedBaseUrl}`,
    );
  }

  if (channelId === "staging") {
    const parsed = parseStagingVersion(version);
    if (!parsed) {
      throw new Error(`invalid staging version: ${version}`);
    }
    if (tag !== stagingTag(version)) {
      throw new Error(`staging tag ${tag} does not select version ${version}`);
    }
    return normalizedBaseUrl;
  }

  const parsedTag = parseReleaseTag(tag);
  if (!parsedTag || parsedTag.version !== version) {
    throw new Error(`release tag ${tag} does not select version ${version}`);
  }
  return normalizedBaseUrl;
}

export function createReleaseManifests({
  dist,
  version,
  tag,
  sha,
  publishedAt,
  baseUrl,
  channel = "production",
}) {
  const normalizedBaseUrl = assertChannelVersion({
    channelId: channel,
    version,
    tag,
    baseUrl,
  });
  if (!/^[0-9a-f]{40}$/.test(sha)) {
    throw new Error("release commit must be a full lowercase SHA-1");
  }
  if (Number.isNaN(Date.parse(publishedAt))) {
    throw new Error(`invalid release publication date: ${publishedAt}`);
  }
  const distPath = path.resolve(dist);
  const artifacts = [];
  const platforms = releasePlatforms(channel);

  for (const platformDescriptor of platforms) {
    for (const arch of platformDescriptor.architectures) {
      const directory = path.join(distPath, platformDescriptor.platform, arch);
      const baseName = `Tidebreak_${version}_${arch}`;

      for (const descriptor of platformDescriptor.formats) {
        const filename = `${baseName}${descriptor.extension}`;
        const file = path.join(directory, filename);
        requireFile(file);

        const digest = sha256(file);
        writeFileSync(`${file}.sha256`, `${digest}  ${filename}\n`);
        const relativeFilename = path.posix.join(
          platformDescriptor.platform,
          arch,
          filename,
        );
        const checksumFilename = `${relativeFilename}.sha256`;
        const artifact = {
          platform: platformDescriptor.platform,
          arch,
          format: descriptor.format,
          filename: relativeFilename,
          url: publicUrl(normalizedBaseUrl, version, relativeFilename),
          size: statSync(file).size,
          sha256: digest,
          checksum_filename: checksumFilename,
          checksum_url: publicUrl(
            normalizedBaseUrl,
            version,
            checksumFilename,
          ),
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
        }

        artifacts.push(artifact);
      }
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
  const latest = createLatestDocument({
    version,
    publishedAt,
    artifacts,
    platforms,
  });

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
    channel: options.get("channel") || "production",
  });
  console.log(JSON.stringify(result.manifest, null, 2));
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  main();
}
