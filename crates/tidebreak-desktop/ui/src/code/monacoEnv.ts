import { loader } from "@monaco-editor/react";
// monaco-editor 0.56 remaps `*.js` → `esm/vs/*.js`, so these paths must not
// include the `esm/vs/` prefix. `monaco-editor` itself is the full barrel.
import * as monaco from "monaco-editor/editor/editor.api";
import editorWorker from "monaco-editor/editor/editor.worker.js?worker";
import jsonWorker from "monaco-editor/language/json/json.worker.js?worker";

// Highlighters only — the full barrel pulls every language plus the LSP client
// into Vite's production transform and OOMs a 4 GB heap.
import "monaco-editor/languages/definitions/css/register.js";
import "monaco-editor/languages/definitions/go/register.js";
import "monaco-editor/languages/definitions/html/register.js";
import "monaco-editor/languages/definitions/ini/register.js";
import "monaco-editor/languages/definitions/java/register.js";
import "monaco-editor/languages/definitions/javascript/register.js";
import "monaco-editor/languages/definitions/kotlin/register.js";
import "monaco-editor/languages/definitions/less/register.js";
import "monaco-editor/languages/definitions/markdown/register.js";
import "monaco-editor/languages/definitions/python/register.js";
import "monaco-editor/languages/definitions/ruby/register.js";
import "monaco-editor/languages/definitions/rust/register.js";
import "monaco-editor/languages/definitions/scss/register.js";
import "monaco-editor/languages/definitions/shell/register.js";
import "monaco-editor/languages/definitions/sql/register.js";
import "monaco-editor/languages/definitions/swift/register.js";
import "monaco-editor/languages/definitions/typescript/register.js";
import "monaco-editor/languages/definitions/xml/register.js";
import "monaco-editor/languages/definitions/yaml/register.js";
import "monaco-editor/language/json/monaco.contribution.js";

let configured = false;

/** Bundle Monaco workers so the desktop app does not fetch them from a CDN. */
export function configureMonaco(): void {
  if (configured) return;
  configured = true;
  self.MonacoEnvironment = {
    getWorker(_id: string, label: string) {
      // CSS/HTML/TS highlighters are Monarch grammars and use the editor
      // worker. The TypeScript language-service worker ships the compiler
      // and blows the production-build heap.
      if (label === "json") return new jsonWorker();
      return new editorWorker();
    },
  };
  loader.config({ monaco });
}

export function monacoLanguage(path: string): string {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  const languages: Record<string, string> = {
    rs: "rust",
    ts: "typescript",
    tsx: "typescript",
    js: "javascript",
    jsx: "javascript",
    mjs: "javascript",
    cjs: "javascript",
    json: "json",
    md: "markdown",
    py: "python",
    go: "go",
    rb: "ruby",
    java: "java",
    kt: "kotlin",
    swift: "swift",
    css: "css",
    scss: "scss",
    less: "less",
    html: "html",
    htm: "html",
    xml: "xml",
    yaml: "yaml",
    yml: "yaml",
    toml: "ini",
    sh: "shell",
    bash: "shell",
    zsh: "shell",
    sql: "sql",
    cssx: "css",
  };
  return languages[ext] ?? "plaintext";
}

export function monacoTheme(): "vs-dark" | "vs" {
  return document.documentElement.classList.contains("dark") ? "vs-dark" : "vs";
}
