import tseslint from "typescript-eslint";

export default tseslint.config(
  {
    ignores: [
      "node_modules/**",
      ".expo/**",
      "dist/**",
      "babel.config.js",
      "metro.config.js",
      "tailwind.config.js",
    ],
  },
  ...tseslint.configs.recommended,
  {
    files: ["**/*.ts", "**/*.tsx"],
    rules: {
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
      // Dev tooling polyfills web globals that a Hermes release build may not
      // have (global crypto crashed on-device pairing; #3292). Only CI can
      // catch a stray reference before the device does.
      "no-restricted-globals": [
        "error",
        {
          name: "crypto",
          message: "Hermes has no global crypto - use expo-crypto.",
        },
        {
          name: "btoa",
          message:
            "Engine coverage varies - use the base64url helpers in src/lib/crypto.ts.",
        },
        {
          name: "atob",
          message: "Engine coverage varies - decode base64 in pure JS.",
        },
      ],
    },
  },
);
