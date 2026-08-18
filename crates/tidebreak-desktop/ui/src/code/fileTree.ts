/**
 * Nested worktree paths and VS Code-style include/exclude globs for the
 * Files explorer.
 */

export type FileTreeNode = {
  kind: "dir" | "file";
  name: string;
  path: string;
  children?: FileTreeNode[];
};

/** Build a directory tree from slash-separated relative paths. */
export function buildFileTree(paths: readonly string[]): FileTreeNode[] {
  const root: FileTreeNode[] = [];
  for (const raw of paths) {
    const parts = raw.split("/").filter((part) => part.length > 0);
    if (parts.length === 0) continue;
    let siblings = root;
    let prefix = "";
    for (let index = 0; index < parts.length; index += 1) {
      const name = parts[index];
      prefix = prefix ? `${prefix}/${name}` : name;
      const isFile = index === parts.length - 1;
      let node = siblings.find((entry) => entry.name === name);
      if (!node) {
        node = isFile
          ? { kind: "file", name, path: prefix }
          : { kind: "dir", name, path: prefix, children: [] };
        siblings.push(node);
      } else if (!isFile && node.kind === "file") {
        node = { kind: "dir", name, path: prefix, children: [] };
        const slot = siblings.findIndex((entry) => entry.name === name);
        siblings[slot] = node;
      }
      if (node.kind === "dir") {
        node.children ??= [];
        siblings = node.children;
      }
    }
  }
  sortTree(root);
  return root;
}

function sortTree(nodes: FileTreeNode[]): void {
  nodes.sort((left, right) => {
    if (left.kind !== right.kind) return left.kind === "dir" ? -1 : 1;
    return left.name.localeCompare(right.name);
  });
  for (const node of nodes) {
    if (node.children) sortTree(node.children);
  }
}

/** Keep paths that match include (if set) and miss every exclude pattern. */
export function filterPaths(
  paths: readonly string[],
  include: string,
  exclude: string,
): string[] {
  return paths.filter(
    (path) => matchesAnyGlob(path, include, true) && !matchesAnyGlob(path, exclude, false),
  );
}

/** Comma-separated globs. An empty include list matches everything. */
export function matchesAnyGlob(
  path: string,
  spec: string,
  emptyMatches: boolean,
): boolean {
  const patterns = spec
    .split(",")
    .map((part) => part.trim())
    .filter((part) => part.length > 0);
  if (patterns.length === 0) return emptyMatches;
  return patterns.some((pattern) => matchGlob(path, pattern));
}

/**
 * Match a relative path against one glob. `*` is one segment, `**` is any
 * depth, and a pattern without a slash also matches the basename.
 */
export function matchGlob(path: string, pattern: string): boolean {
  const normalized = pattern.replace(/\\/g, "/").replace(/^\/+/, "");
  if (!normalized) return false;
  const regex = globToRegExp(normalized);
  if (regex.test(path)) return true;
  if (!normalized.includes("/")) {
    const base = path.split("/").pop() ?? path;
    return regex.test(base);
  }
  return false;
}

function globToRegExp(pattern: string): RegExp {
  let source = "^";
  for (let index = 0; index < pattern.length; ) {
    const char = pattern[index];
    if (char === "*" && pattern[index + 1] === "*") {
      const next = pattern[index + 2];
      if (next === "/") {
        source += "(?:.*/)?";
        index += 3;
        continue;
      }
      source += ".*";
      index += 2;
      continue;
    }
    if (char === "*") {
      source += "[^/]*";
      index += 1;
      continue;
    }
    if (char === "?") {
      source += "[^/]";
      index += 1;
      continue;
    }
    if ("\\^$+{}[]()|.".includes(char)) source += `\\${char}`;
    else source += char;
    index += 1;
  }
  source += "$";
  return new RegExp(source, "i");
}

/** Directory paths that contain a matching file, so search can expand them. */
export function ancestorPaths(path: string): string[] {
  const parts = path.split("/").filter((part) => part.length > 0);
  const out: string[] = [];
  let prefix = "";
  for (let index = 0; index < parts.length - 1; index += 1) {
    prefix = prefix ? `${prefix}/${parts[index]}` : parts[index];
    out.push(prefix);
  }
  return out;
}
