#!/usr/bin/env node

import { appendFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

export const PRODUCTION_BASE_URL = "https://downloads.brightwave.io/tidebreak";
export const STAGING_BASE_URL = `${PRODUCTION_BASE_URL}/staging`;

export const DESKTOP_CHANNELS = Object.freeze({
  production: Object.freeze({
    id: "production",
    identifier: "io.brightwave.tidebreak",
    productName: "Tidebreak",
    scheme: "tidebreak",
    environment: "desktop-production",
    baseUrl: PRODUCTION_BASE_URL,
    s3Prefix: "tidebreak",
    updaterEndpoint: `${PRODUCTION_BASE_URL}/latest.json`,
  }),
  staging: Object.freeze({
    id: "staging",
    identifier: "io.brightwave.tidebreak.staging",
    productName: "Tidebreak [staging]",
    scheme: "tidebreak-staging",
    environment: "desktop-staging",
    baseUrl: STAGING_BASE_URL,
    s3Prefix: "tidebreak/staging",
    updaterEndpoint: `${STAGING_BASE_URL}/latest.json`,
  }),
});

export function desktopChannel(id) {
  const channel = DESKTOP_CHANNELS[id];
  if (!channel) {
    throw new Error(`unknown desktop channel: ${id}`);
  }
  return channel;
}

export function assertHostedUnderChannel(key, channelId) {
  const channel = desktopChannel(channelId);
  if (channelId === "staging" && key.startsWith("tidebreak/releases/")) {
    throw new Error(`staging must not write production release objects: ${key}`);
  }
  if (
    channelId === "staging" &&
    (key === "tidebreak/latest.json" || key === "tidebreak/manifest.json")
  ) {
    throw new Error(`staging must not write production feed objects: ${key}`);
  }
  const prefix = `${channel.s3Prefix}/`;
  if (key !== channel.s3Prefix && !key.startsWith(prefix)) {
    throw new Error(
      `refusing to publish ${key} outside ${channel.s3Prefix}/ for ${channelId}`,
    );
  }
}

function main() {
  if (process.argv[2] === "--assert-key") {
    assertHostedUnderChannel(process.argv[4], process.argv[3]);
    return;
  }
  const id = process.argv[2];
  if (!id) {
    throw new Error(
      "usage: desktop-channel.mjs <production|staging> | --assert-key <channel> <key>",
    );
  }
  const channel = desktopChannel(id);
  if (process.env.GITHUB_OUTPUT) {
    appendFileSync(
      process.env.GITHUB_OUTPUT,
      Object.entries(channel)
        .map(([key, value]) => `${key}=${value}\n`)
        .join(""),
    );
  }
  console.log(JSON.stringify(channel, null, 2));
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  main();
}
