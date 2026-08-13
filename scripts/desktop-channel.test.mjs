import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  DESKTOP_CHANNELS,
  PRODUCTION_BASE_URL,
  STAGING_BASE_URL,
  assertHostedUnderChannel,
  desktopChannel,
} from "./desktop-channel.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

test("production and staging do not share a host prefix or feed", () => {
  const production = desktopChannel("production");
  const staging = desktopChannel("staging");

  assert.equal(production.baseUrl, PRODUCTION_BASE_URL);
  assert.equal(staging.baseUrl, STAGING_BASE_URL);
  assert.ok(staging.baseUrl.startsWith(`${production.baseUrl}/`));
  assert.notEqual(production.identifier, staging.identifier);
  assert.notEqual(production.scheme, staging.scheme);
  assert.notEqual(production.environment, staging.environment);
  assert.equal(staging.s3Prefix, "tidebreak/staging");
  assert.equal(production.updaterEndpoint, `${PRODUCTION_BASE_URL}/latest.json`);
  assert.equal(staging.updaterEndpoint, `${STAGING_BASE_URL}/latest.json`);
  assert.throws(() => desktopChannel("nightly"), /unknown desktop channel/);
});

test("staging hosting cannot address production objects", () => {
  assert.doesNotThrow(() =>
    assertHostedUnderChannel("tidebreak/staging/latest.json", "staging"),
  );
  assert.doesNotThrow(() =>
    assertHostedUnderChannel(
      "tidebreak/staging/releases/v0.0.0-staging.1/manifest.json",
      "staging",
    ),
  );
  assert.throws(
    () => assertHostedUnderChannel("tidebreak/latest.json", "staging"),
    /production feed/,
  );
  assert.throws(
    () =>
      assertHostedUnderChannel("tidebreak/releases/v0.4.2/manifest.json", "staging"),
    /production release/,
  );
  assert.throws(
    () => assertHostedUnderChannel("other/latest.json", "staging"),
    /outside tidebreak\/staging/,
  );
});

test("the packaged staging overlay matches the channel contract", () => {
  const overlay = JSON.parse(
    readFileSync(
      join(root, "crates", "tidebreak-desktop", "tauri.staging.conf.json"),
      "utf8",
    ),
  );
  const staging = DESKTOP_CHANNELS.staging;
  assert.equal(overlay.identifier, staging.identifier);
  assert.equal(overlay.productName, staging.productName);
  assert.deepEqual(overlay.plugins["deep-link"].desktop.schemes, [
    staging.scheme,
  ]);
  assert.deepEqual(overlay.plugins.updater.endpoints, [staging.updaterEndpoint]);
  assert.ok(
    overlay.bundle.icon.every((icon) => icon.startsWith("icons/staging/")),
  );
});
