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
      // The gateway console's pairing link. Carries a gateway URL and a
      // claimable pairing-session handle, never a credential (mg ADR 0086),
      // so it is safe to accept from any source — `src/lib/provision.ts` is
      // what decides whether a given payload is one of ours.
      {
        action: "VIEW",
        autoVerify: false,
        data: [{ scheme: SCHEME[VARIANT], host: "provision" }],
        category: ["BROWSABLE", "DEFAULT"],
      },
    ],
  },
  plugins: [
    "expo-router",
    "expo-secure-store",
    "expo-web-browser",
    // Scanning a pairing or attach QR. The permission string is what the OS
    // shows in the prompt, so it names the one thing the camera is for. It
    // covers both codes now: the gateway console's pairing code, and the
    // standalone machine's attach code (#3404, decision 98).
    [
      "expo-camera",
      {
        cameraPermission:
          "Tidebreak uses the camera only to scan the code that connects this phone to your gateway or your own Tidebreak machine.",
        recordAudioAndroid: false,
      },
    ],
    // Push. No `googleServicesFile` yet: without a Firebase config an Android
    // build has no FCM registration, so it registers its Expo token, never
    // claims it can render data-only messages, and receives ordinary
    // display-form notifications. iOS is unaffected.
    "expo-notifications",
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
