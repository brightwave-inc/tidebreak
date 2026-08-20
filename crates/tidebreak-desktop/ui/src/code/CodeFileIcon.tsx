import type { LucideIcon } from "lucide-react";
import {
  BookOpenText,
  Braces,
  Database,
  File,
  FileCode2,
  FileCog,
  FileImage,
  FileJson,
  FileText,
  FileType2,
  LockKeyhole,
  Package,
  Palette,
} from "lucide-react";

import { cn } from "@/lib/utils";

type FileIconSpec = {
  icon: LucideIcon;
  tone: string;
  kind: string;
};

const CODE_EXTENSIONS = new Set([
  "c",
  "cc",
  "cpp",
  "cs",
  "go",
  "h",
  "hpp",
  "java",
  "js",
  "jsx",
  "kt",
  "lua",
  "m",
  "php",
  "py",
  "rb",
  "rs",
  "swift",
  "ts",
  "tsx",
  "vue",
]);

const TEXT_EXTENSIONS = new Set([
  "md",
  "mdx",
  "rst",
  "txt",
  "adoc",
]);

const CONFIG_EXTENSIONS = new Set([
  "env",
  "ini",
  "properties",
  "toml",
  "yaml",
  "yml",
]);

const IMAGE_EXTENSIONS = new Set([
  "avif",
  "gif",
  "ico",
  "jpeg",
  "jpg",
  "png",
  "svg",
  "webp",
]);

const STYLE_EXTENSIONS = new Set(["css", "less", "scss", "sass"]);
const DATA_EXTENSIONS = new Set(["db", "sqlite", "sqlite3", "sql"]);

/**
 * A quiet file-type glyph shared by file navigation and changed-file trees.
 * The color is an identity cue, never a status cue; callers can override it
 * with `className` when Git state is the more important signal.
 */
export function CodeFileIcon({
  path,
  className,
}: {
  path: string;
  className?: string;
}) {
  const spec = fileIconSpec(path);
  const Icon = spec.icon;
  return (
    <Icon
      className={cn("size-3.5 shrink-0", spec.tone, className)}
      data-file-icon={spec.kind}
      aria-hidden
    />
  );
}

export function fileIconSpec(path: string): FileIconSpec {
  const name = path.split("/").pop()?.toLowerCase() ?? path.toLowerCase();
  const extension = name.includes(".") ? (name.split(".").pop() ?? "") : "";

  if (name.startsWith("readme")) {
    return {
      icon: BookOpenText,
      tone: "text-info-foreground",
      kind: "readme",
    };
  }
  if (
    name === "license" ||
    name.startsWith("license.") ||
    name.startsWith("changelog")
  ) {
    return { icon: FileText, tone: "text-muted-foreground", kind: "text" };
  }
  if (
    name === "package.json" ||
    name === "cargo.toml" ||
    name === "pyproject.toml" ||
    name === "go.mod"
  ) {
    return {
      icon: Package,
      tone: "text-warning-foreground",
      kind: "package",
    };
  }
  if (name.endsWith(".lock") || name.includes("lock.")) {
    return {
      icon: LockKeyhole,
      tone: "text-muted-foreground",
      kind: "lock",
    };
  }
  if (extension === "json" || extension === "jsonc") {
    return {
      icon: FileJson,
      tone: "text-warning-foreground",
      kind: "json",
    };
  }
  if (CODE_EXTENSIONS.has(extension)) {
    return { icon: FileCode2, tone: "text-info-foreground", kind: "code" };
  }
  if (TEXT_EXTENSIONS.has(extension)) {
    return { icon: FileText, tone: "text-muted-foreground", kind: "text" };
  }
  if (CONFIG_EXTENSIONS.has(extension) || name.startsWith(".env")) {
    return {
      icon: FileCog,
      tone: "text-warning-foreground",
      kind: "config",
    };
  }
  if (IMAGE_EXTENSIONS.has(extension)) {
    return { icon: FileImage, tone: "text-violet-500", kind: "image" };
  }
  if (STYLE_EXTENSIONS.has(extension)) {
    return { icon: Palette, tone: "text-pink-500", kind: "style" };
  }
  if (DATA_EXTENSIONS.has(extension)) {
    return { icon: Database, tone: "text-teal-500", kind: "data" };
  }
  if (extension === "html" || extension === "xml") {
    return { icon: Braces, tone: "text-orange-500", kind: "markup" };
  }
  if (extension === "woff" || extension === "woff2" || extension === "ttf") {
    return { icon: FileType2, tone: "text-violet-500", kind: "font" };
  }
  return { icon: File, tone: "text-muted-foreground", kind: "file" };
}
