/**
 * Which highlight.js grammar a source file reads as, by its name.
 *
 * Shared by the output source viewer and the code-mode diff, so a file is
 * read as the same language wherever it is shown. Names outside this list
 * return null and render as plain text.
 */
const LANGUAGE_BY_EXTENSION: Readonly<Record<string, string>> = {
  py: "python",
  pyw: "python",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  ts: "typescript",
  tsx: "typescript",
  mts: "typescript",
  cts: "typescript",
  rs: "rust",
  go: "go",
  java: "java",
  c: "c",
  h: "c",
  cc: "cpp",
  cpp: "cpp",
  cxx: "cpp",
  hpp: "cpp",
  hxx: "cpp",
  cs: "csharp",
  rb: "ruby",
  php: "php",
  swift: "swift",
  kt: "kotlin",
  kts: "kotlin",
  sh: "bash",
  bash: "bash",
  zsh: "bash",
  fish: "bash",
  sql: "sql",
  css: "css",
  scss: "scss",
  sass: "scss",
  less: "less",
  vue: "xml",
  svelte: "xml",
  toml: "ini",
  yaml: "yaml",
  yml: "yaml",
  xml: "xml",
  graphql: "graphql",
  gql: "graphql",
  lua: "lua",
  r: "r",
  pl: "perl",
  pm: "perl",
};

/** Highlight language for a source filename, when the extension identifies one. */
export function codeLanguageForFilename(filename: string): string | null {
  const lower = filename.toLowerCase();
  if (["dockerfile", "makefile", "justfile"].includes(lower)) {
    return lower === "dockerfile" ? "bash" : "makefile";
  }
  const extension = lower.includes(".") ? lower.split(".").pop()! : "";
  return LANGUAGE_BY_EXTENSION[extension] ?? null;
}
