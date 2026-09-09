import type { ConfigContext, ExpoConfig } from "expo/config";

type AppVariant = "production" | "staging" | "development";

const VARIANT: AppVariant =
  process.env.APP_VARIANT === "staging"
    ? "staging"
    : process.env.APP_VARIANT === "development"
      ? "development"
      : "production";

const SCHEME: Record<AppVariant, string> = {
  production: "tidebreak",
  staging: "tidebreak-staging",
  development: "tidebreak-dev",
};

// Printed by `eas init` (project @brightwave/tidebreak-mobile); eas-cli cannot
// write into a dynamic (TS) config, so the id is pasted here by hand. If it is
// ever emptied, the EAS/updates fields are omitted entirely — a placeholder
// value makes `eas init` believe the project is already linked and fail.
const EAS_PROJECT_ID = "af9811ba-f747-44e9-b5cd-c1fe33b4c6e9";

export default ({ config }: ConfigContext): ExpoConfig => ({
  ...config,
  name:
    VARIANT === "production"
      ? "Tidebreak"
      : VARIANT === "staging"
        ? "Tidebreak Staging"
        : "Tidebreak Dev",
  slug: "tidebreak-mobile",
  owner: "brightwave",
  version: "0.1.0",
  orientation: "portrait",
  scheme: SCHEME[VARIANT],
  userInterfaceStyle: "automatic",
  icon: "./assets/icon.png",
  // OTA updates (EAS Update). The fingerprint policy hashes every
  // native-relevant input, so an OTA only ever applies to binaries whose
  // native hash matches — JS-only changes ship over the air, native changes
  // force a store build. Keep this config deterministic: a value that changes
  // between runs breaks fingerprint routing.
  runtimeVersion: { policy: "fingerprint" },
  ...(EAS_PROJECT_ID
    ? {
        updates: {
          url: `https://u.expo.dev/${EAS_PROJECT_ID}`,
          requestHeaders: {
            "expo-channel-name":
              VARIANT === "production"
                ? "production"
                : VARIANT === "staging"
                  ? "staging"
                  : "development",
          },
        },
      }
    : {}),
  ios: {
    supportsTablet: true,
    appleTeamId: "CUURNS78Y4",
    bundleIdentifier:
      VARIANT === "production"
        ? "inc.brightwave.tidebreak"
        : VARIANT === "staging"
          ? "inc.brightwave.tidebreak.staging"
          : "inc.brightwave.tidebreak.dev",
    infoPlist: {
      // Pre-answers export compliance; the app uses only HTTPS-exempt
      // encryption. Without this every TestFlight build waits on the
      // questionnaire.
      ITSAppUsesNonExemptEncryption: false,
    },
  },
  android: {
    package:
      VARIANT === "production"
        ? "inc.brightwave.tidebreak"
        : VARIANT === "staging"
          ? "inc.brightwave.tidebreak.staging"
          : "inc.brightwave.tidebreak.dev",
    adaptiveIcon: {
      backgroundColor: "#F7F8FA",
    },
    intentFilters: [
      {
        action: "VIEW",
        autoVerify: false,
        data: [{ scheme: SCHEME[VARIANT], host: "callback" }],
        category: ["BROWSABLE", "DEFAULT"],
      },
    ],
  },
  plugins: [
    "expo-router",
    "expo-secure-store",
    "expo-web-browser",
  ],
  extra: {
    appVariant: VARIANT,
    oauthRedirectUri: `${SCHEME[VARIANT]}://callback`,
    ...(EAS_PROJECT_ID
      ? {
          eas: {
            projectId: EAS_PROJECT_ID,
          },
        }
      : {}),
  },
});
