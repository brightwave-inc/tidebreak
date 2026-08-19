import { loader } from "@monaco-editor/react";
// editor.api is the lean standalone editor surface. It skips language services
// and their extra workers, which previously pushed the CI production build out
// of memory; syntax grammars are registered separately below.
import * as monaco from "monaco-editor/editor/editor.api.js";
import editorWorker from "monaco-editor/editor/editor.worker.js?worker";
// The lean editor API does not register language grammars by itself. Register
// only the grammars the file viewer can select; each definition stays lazy, so
// opening a Rust file does not eagerly download every tokenizer Monaco ships.
import "monaco-editor/languages/definitions/css/register.js";
import "monaco-editor/languages/definitions/dockerfile/register.js";
import "monaco-editor/languages/definitions/go/register.js";
import "monaco-editor/languages/definitions/html/register.js";
import "monaco-editor/languages/definitions/ini/register.js";
import "monaco-editor/languages/definitions/java/register.js";
import "monaco-editor/languages/definitions/javascript/register.js";
import "monaco-editor/languages/definitions/kotlin/register.js";
import "monaco-editor/languages/definitions/less/register.js";
import "monaco-editor/languages/definitions/markdown/register.js";
import "monaco-editor/languages/definitions/php/register.js";
import "monaco-editor/languages/definitions/powershell/register.js";
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

import type { ResolvedTheme } from "@/theme";

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
  const name = path.split(/[\\/]/).pop()?.toLowerCase() ?? "";
  const filenames: Record<string, string> = {
    dockerfile: "dockerfile",
    "dockerfile.dev": "dockerfile",
    "dockerfile.prod": "dockerfile",
    makefile: "shell",
    justfile: "shell",
    ".bashrc": "shell",
    ".zshrc": "shell",
    ".gitignore": "ini",
    ".gitattributes": "ini",
    ".editorconfig": "ini",
  };
  const named = filenames[name];
  if (named) return named;
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
    ps1: "powershell",
    php: "php",
    env: "ini",
  };
  return languages[ext] ?? "plaintext";
}

export function monacoTheme(theme: ResolvedTheme): "vs-dark" | "vs" {
  return theme === "dark" ? "vs-dark" : "vs";
}
