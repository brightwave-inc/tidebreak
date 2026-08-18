/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "path";
import { tidebreakDevListenPlugin } from "./vite-dev-listen";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

const monacoWorkerStub = path.resolve(__dirname, "./src/test/monacoWorkerStub.ts");

// Vitest cannot resolve Monaco's `?worker` imports. Production still
// bundles the real workers; tests get a constructor stub.
function stubMonacoWorkersForVitest() {
  return {
    name: "stub-monaco-workers-for-vitest",
    enforce: "pre" as const,
    resolveId(id: string) {
      if (!process.env.VITEST || !id.includes("monaco-editor")) return undefined;
      if (id.includes("?worker") || id.includes(".worker.js")) {
        return monacoWorkerStub;
      }
      return undefined;
    },
  };
}

export default defineConfig(async () => ({
  plugins: [
    stubMonacoWorkersForVitest(),
    react(),
    tailwindcss(),
    tidebreakDevListenPlugin(),
  ],
  // Extend resolves parser workers relative to each package entry point. Vite's
  // dependency optimizer rewrites the entry point into node_modules/.vite but
  // does not copy those sibling worker modules, so keep the worker-owning
  // viewers as source dependencies in development.
  optimizeDeps: {
    exclude: [
      "@extend-ai/react-docx",
      "@extend-ai/react-pptx",
      "@extend-ai/react-xlsx",
    ],
    // The unbundled viewers import a few CommonJS leaves. Prebundle those
    // leaves so Vite provides the ESM exports the packages expect while
    // leaving each worker-owning viewer itself untouched.
    include: ["react-dom/server", "regl", "utif"],
  },
  // The spreadsheet viewer parses and calculates off the main thread. Those
  // workers are large enough to be split into chunks themselves, which the
  // default IIFE worker format cannot express.
  worker: { format: "es" as const },
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  test: {
    // Pure-logic and SSR-markup tests run in node; DOM interaction tests opt
    // in per file with an `@vitest-environment jsdom` docblock.
    environment: "node",
    setupFiles: ["./src/test/setup.ts"],
    alias: {
      "monaco-editor/editor/editor.worker.js?worker": monacoWorkerStub,
      "monaco-editor/language/css/css.worker.js?worker": monacoWorkerStub,
      "monaco-editor/language/html/html.worker.js?worker": monacoWorkerStub,
      "monaco-editor/language/json/json.worker.js?worker": monacoWorkerStub,
      "monaco-editor/language/typescript/ts.worker.js?worker": monacoWorkerStub,
    },
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      ignored: ["**/src-tauri/**", "**/target/**"],
    },
  },
}));
