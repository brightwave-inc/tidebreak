/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import fs from "node:fs";
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

function collectTestFiles(dir: string, acc: string[] = []): string[] {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === "node_modules") continue;
      collectTestFiles(full, acc);
    } else if (/\.test\.(ts|tsx)$/.test(entry.name)) {
      acc.push(full);
    }
  }
  return acc;
}

function isJsdomTest(file: string): boolean {
  if (file.includes(".dom.test.")) return true;
  const head = fs.readFileSync(file, "utf8").slice(0, 800);
  return head.includes("@vitest-environment jsdom");
}

const uiRoot = __dirname;
const allTestFiles = [
  ...collectTestFiles(path.join(uiRoot, "src")),
  path.join(uiRoot, "vite-dev-listen.test.ts"),
].filter((file) => fs.existsSync(file));
const jsdomTestFiles = allTestFiles
  .filter(isJsdomTest)
  .map((file) => path.relative(uiRoot, file));
const nodeTestFiles = allTestFiles
  .filter((file) => !isJsdomTest(file))
  .map((file) => path.relative(uiRoot, file));

function katexWoff2Only() {
  const isKatexFont = (fileName: string) =>
    fileName.includes("KaTeX_") &&
    (fileName.endsWith(".ttf") ||
      (fileName.endsWith(".woff") && !fileName.endsWith(".woff2")));

  return {
    name: "katex-woff2-only",
    enforce: "pre" as const,
    transform(code: string, id: string) {
      if (!id.includes("katex") || !id.includes(".css")) {
        return undefined;
      }
      return {
        code: code
          .replace(/,url\([^)]+\.woff\) format\("woff"\)/g, "")
          .replace(/,url\([^)]+\.ttf\) format\("truetype"\)/g, ""),
        map: null,
      };
    },
    generateBundle(
      _options: unknown,
      bundle: Record<
        string,
        { type: string; source?: string | Uint8Array; fileName: string }
      >,
    ) {
      for (const [fileName, file] of Object.entries(bundle)) {
        if (isKatexFont(fileName)) {
          delete bundle[fileName];
          continue;
        }
        if (
          file.type === "asset" &&
          fileName.endsWith(".css") &&
          typeof file.source === "string" &&
          file.source.includes("KaTeX_")
        ) {
          file.source = file.source
            .replace(/url\([^)]+\.woff\) format\("woff"\),?/g, "")
            .replace(/url\([^)]+\.ttf\) format\("truetype"\),?/g, "")
            .replace(/,,/g, ",")
            .replace(/,\s*}/g, "}");
        }
      }
    },
  };
}

function stubUniverEngineRenderInWorkers() {
  const stub = path.resolve(__dirname, "./src/workers/engine-render-stub.ts");
  return {
    name: "stub-univer-engine-render-in-workers",
    enforce: "pre" as const,
    resolveId(id: string) {
      return id === "@univerjs/engine-render" ? stub : undefined;
    },
  };
}

export default defineConfig(async () => ({
  plugins: [
    stubMonacoWorkersForVitest(),
    katexWoff2Only(),
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
  worker: {
    format: "es" as const,
    plugins: () => [stubUniverEngineRenderInWorkers()],
  },
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  test: {
    setupFiles: ["./src/test/setup.ts"],
    // Vitest 4's restoreAllMocks only undoes spyOn; vi.fn() call history
    // otherwise leaks from one case into the next.
    clearMocks: true,
    pool: "threads",
    fileParallelism: true,
    // Fail a slow test rather than letting waitFor hang the lane.
    testTimeout: 4000,
    alias: {
      "monaco-editor/editor/editor.worker.js?worker": monacoWorkerStub,
    },
    projects: [
      {
        extends: true,
        test: {
          name: "node",
          environment: "node",
          isolate: false,
          pool: "threads",
          include: nodeTestFiles,
        },
      },
      {
        extends: true,
        test: {
          name: "dom",
          environment: "jsdom",
          // jsdom is not thread-safe here; forks keep each file's document
          // off the shared worker.
          pool: "forks",
          include: jsdomTestFiles,
        },
      },
    ],
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
