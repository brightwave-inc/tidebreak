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

/** Row indent per nesting level, and the depth past which it stops growing. */
const INDENT_BASE_PX = 8;
const INDENT_STEP_PX = 12;
const MAX_INDENT_DEPTH = 8;

/**
 * Left padding for a tree row at `depth`.
 *
 * The indent stops growing at eight levels. The explorer lives in a rail a few
 * hundred pixels wide, and past that depth each further step trades a readable
 * file name for a nesting level the rows above already show.
 */
export function treeIndentPx(depth: number): number {
  return INDENT_BASE_PX + Math.min(depth, MAX_INDENT_DEPTH) * INDENT_STEP_PX;
}

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

/** One row the explorer currently draws, with what arrow keys need to move. */
export type VisibleTreeRow = {
  node: FileTreeNode;
  depth: number;
  /** Owning directory path, or null at the root. */
  parent: string | null;
  expanded: boolean;
};

/**
 * The tree as the reader sees it: one entry per drawn row, in document order.
 *
 * Arrow-key navigation moves between rows rather than between siblings, so it
 * needs the collapsed subtrees already gone and each row's depth and parent
 * carried alongside. Deriving that here keeps the key handler free of the
 * recursion the renderer does.
 */
export function flattenVisibleTree(
  nodes: readonly FileTreeNode[],
  isOpen: (path: string) => boolean,
): VisibleTreeRow[] {
  const rows: VisibleTreeRow[] = [];
  const walk = (
    list: readonly FileTreeNode[],
    depth: number,
    parent: string | null,
  ) => {
    for (const node of list) {
      const expanded = node.kind === "dir" && isOpen(node.path);
      rows.push({ node, depth, parent, expanded });
      if (expanded && node.children) walk(node.children, depth + 1, node.path);
    }
  };
  walk(nodes, 0, null);
  return rows;
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
