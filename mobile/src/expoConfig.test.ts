import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const mobileRoot = join(here, "..");
const expoCli = join(mobileRoot, "node_modules/expo/bin/cli");

type PublicExpoConfig = {
  scheme?: string;
  android?: {
    intentFilters?: Array<{
      action?: string;
      data?: Array<{ scheme?: string; host?: string }>;
      category?: string[];
    }>;
  };
  extra?: {
    oauthRedirectUri?: string;
  };
};

function resolvePublicConfig(appVariant: string): PublicExpoConfig {
  const result = spawnSync(
    process.execPath,
    [expoCli, "config", "--type", "public", "--json"],
    {
      cwd: mobileRoot,
      encoding: "utf8",
      env: { ...process.env, APP_VARIANT: appVariant },
    },
  );

  if (result.status !== 0) {
    throw new Error(
      ["Expo config resolution failed.", result.stdout, result.stderr]
        .filter(Boolean)
        .join("\n"),
    );
  }

  return JSON.parse(result.stdout) as PublicExpoConfig;
}

describe("Expo config", () => {
  it.each([
    ["production", "tidebreak"],
    ["staging", "tidebreak-staging"],
    ["development", "tidebreak-dev"],
  ])(
    "resolves the %s config with its deep-link contract",
    (appVariant, scheme) => {
      const config = resolvePublicConfig(appVariant);

      expect(config.scheme).toBe(scheme);
      expect(config.extra?.oauthRedirectUri).toBe(`${scheme}://callback`);
      expect(config.android?.intentFilters).toContainEqual({
        action: "VIEW",
        autoVerify: false,
        data: [{ scheme, host: "callback" }],
        category: ["BROWSABLE", "DEFAULT"],
      });
    },
  );
});
