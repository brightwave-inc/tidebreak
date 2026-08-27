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

export default ({ config }: ConfigContext): ExpoConfig => ({
  ...config,
  name:
    VARIANT === "production"
      ? "Tidebreak"
      : VARIANT === "staging"
        ? "Tidebreak Staging"
        : "Tidebreak Dev",
  slug: "tidebreak-mobile",
  version: "0.0.0",
  orientation: "portrait",
  scheme: SCHEME[VARIANT],
  userInterfaceStyle: "automatic",
  ios: {
    supportsTablet: true,
    bundleIdentifier:
      VARIANT === "production"
        ? "inc.brightwave.tidebreak"
        : VARIANT === "staging"
          ? "inc.brightwave.tidebreak.staging"
          : "inc.brightwave.tidebreak.dev",
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
  },
});
