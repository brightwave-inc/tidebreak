import bash from "highlight.js/lib/languages/bash";
import css from "highlight.js/lib/languages/css";
import diff from "highlight.js/lib/languages/diff";
import go from "highlight.js/lib/languages/go";
import ini from "highlight.js/lib/languages/ini";
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import markdown from "highlight.js/lib/languages/markdown";
import python from "highlight.js/lib/languages/python";
import rust from "highlight.js/lib/languages/rust";
import sql from "highlight.js/lib/languages/sql";
import typescript from "highlight.js/lib/languages/typescript";
import xml from "highlight.js/lib/languages/xml";
import yaml from "highlight.js/lib/languages/yaml";
import type { Options as RehypeHighlightOptions } from "rehype-highlight";

/**
 * Languages the transcript and output highlighters actually run. Passing this
 * to `rehype-highlight` keeps highlight.js from shipping its common grammar
 * set (thirty-seven languages) on the chat route.
 */
export const highlightLanguages: NonNullable<
  RehypeHighlightOptions["languages"]
> = {
  bash,
  css,
  diff,
  go,
  ini,
  javascript,
  json,
  markdown,
  python,
  rust,
  sql,
  typescript,
  xml,
  yaml,
};

/** Fence tags that should reuse a registered grammar. */
export const highlightAliases: NonNullable<RehypeHighlightOptions["aliases"]> =
  {
    bash: ["sh", "shell"],
    typescript: ["ts", "tsx"],
    javascript: ["js", "jsx"],
    xml: ["html"],
    ini: ["toml"],
    yaml: ["yml"],
    markdown: ["md"],
  };

export const highlightRehypeOptions: RehypeHighlightOptions = {
  detect: false,
  languages: highlightLanguages,
  aliases: highlightAliases,
};
