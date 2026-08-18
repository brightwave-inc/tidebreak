import { loader } from "@monaco-editor/react";
import * as monaco from "monaco-editor";

// monaco-editor 0.56 exports `*.js` as `esm/vs/*.js`, so these must not
// include the `esm/vs/` prefix.
import editorWorker from "monaco-editor/editor/editor.worker.js?worker";
import cssWorker from "monaco-editor/language/css/css.worker.js?worker";
import htmlWorker from "monaco-editor/language/html/html.worker.js?worker";
import jsonWorker from "monaco-editor/language/json/json.worker.js?worker";
import tsWorker from "monaco-editor/language/typescript/ts.worker.js?worker";

let configured = false;

/** Bundle Monaco workers so the desktop app does not fetch them from a CDN. */
export function configureMonaco(): void {
  if (configured) return;
  configured = true;
  self.MonacoEnvironment = {
    getWorker(_id: string, label: string) {
      if (label === "json") return new jsonWorker();
      if (label === "css" || label === "scss" || label === "less") {
        return new cssWorker();
      }
      if (label === "html" || label === "handlebars" || label === "razor") {
        return new htmlWorker();
      }
      if (label === "typescript" || label === "javascript") {
        return new tsWorker();
      }
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
