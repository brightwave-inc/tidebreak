import { loader } from "@monaco-editor/react";
// editor.api is the editor plus tokenizers. It skips the TypeScript language
// service and the extra workers that OOM the CI production build.
import * as monaco from "monaco-editor/editor/editor.api.js";
import editorWorker from "monaco-editor/editor/editor.worker.js?worker";

let configured = false;

/** Bundle the editor worker so the desktop app does not fetch it from a CDN. */
export function configureMonaco(): void {
  if (configured) return;
  configured = true;
  self.MonacoEnvironment = {
    getWorker() {
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
