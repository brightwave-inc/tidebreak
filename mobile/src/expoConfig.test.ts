import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const mobileRoot = join(here, "..");
const expoCli = join(mobileRoot, "node_modules/expo/bin/cli");

type PublicExpoConfig = {
  userInterfaceStyle?: string;
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
  plugins?: Array<string | [string, Record<string, unknown>]>;
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

      expect(config.userInterfaceStyle).toBe("automatic");
      expect(config.scheme).toBe(scheme);
      expect(config.extra?.oauthRedirectUri).toBe(`${scheme}://callback`);
      expect(config.android?.intentFilters).toContainEqual({
        action: "VIEW",
        autoVerify: false,
        data: [{ scheme, host: "callback" }],
        category: ["BROWSABLE", "DEFAULT"],
      });
      // The gateway console's pairing link. Without this filter a scanned or
      // shared provision link opens a browser instead of the app, and QR
      // pairing silently has no way in from outside the camera screen.
      expect(config.android?.intentFilters).toContainEqual({
        action: "VIEW",
        autoVerify: false,
        data: [{ scheme, host: "provision" }],
        category: ["BROWSABLE", "DEFAULT"],
      });
      // Both native surfaces this slice adds must be configured, on every
      // variant: a missing plugin is an app that builds and then cannot ask
      // for the camera or receive a notification.
      const pluginNames = (config.plugins ?? []).map((plugin) =>
        Array.isArray(plugin) ? plugin[0] : plugin,
      );
      expect(pluginNames).toContain("expo-camera");
      expect(pluginNames).toContain("expo-notifications");
    },
  );
});
