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
      // Hermes release builds have no web crypto/base64 globals; dev tooling
      // polyfills them, so only CI can catch a stray reference before device.
      "no-restricted-globals": [
        "error",
        {
          name: "crypto",
          message: "No global crypto in Hermes release builds - use expo-crypto.",
        },
        {
          name: "btoa",
          message: "No btoa in Hermes release builds - encode base64 in JS.",
        },
        {
          name: "atob",
          message: "No atob in Hermes release builds - decode base64 in JS.",
        },
      ],
    },
  },
);
