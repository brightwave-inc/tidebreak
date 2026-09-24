import type { FileChangeKind } from "../api/types";

export type DiffLineKind = "add" | "del" | "context" | "hunk" | "meta";

export type DiffLine = {
  kind: DiffLineKind;
  oldNo: number | null;
  newNo: number | null;
  text: string;
};

export type DiffFileGroup = {
  path: string;
  lines: DiffLine[];
};

const HUNK_HEADER = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/;

/**
 * Split a unified diff into per-file groups of structured lines.
 *
 * Inside a hunk, the first character alone decides add / delete / context.
 * Hunk `@@` counts mark where the hunk ends, so `--- keep` and `+++` are
 * real changed lines, not file headers. File `---` / `+++` lines count as
 * headers only before that file's first hunk.
 */
export function groupUnifiedDiff(diff: string): DiffFileGroup[] {
  const groups: DiffFileGroup[] = [];
  let current: DiffFileGroup | null = null;
  let oldCursor = 0;
  let newCursor = 0;
  let remainingOld = 0;
  let remainingNew = 0;
  let inHunk = false;
  let seenHunk = false;

  function ensureGroup(path: string): DiffFileGroup {
    if (current) return current;
    current = { path, lines: [] };
    groups.push(current);
    return current;
  }

  function endHunkIfConsumed() {
    if (inHunk && remainingOld <= 0 && remainingNew <= 0) {
      inHunk = false;
    }
  }

  for (const line of diff.split("\n")) {
    const file = parseDiffGitLine(line);
    if (file) {
      current = { path: file, lines: [] };
      groups.push(current);
      oldCursor = 0;
      newCursor = 0;
      remainingOld = 0;
      remainingNew = 0;
      inHunk = false;
      seenHunk = false;
      continue;
    }

    if (line.length === 0) continue;

    const group = ensureGroup("file");
    const hunk = HUNK_HEADER.exec(line);
    if (hunk) {
      oldCursor = Number(hunk[1]);
      newCursor = Number(hunk[3]);
      remainingOld = hunk[2] === undefined ? 1 : Number(hunk[2]);
      remainingNew = hunk[4] === undefined ? 1 : Number(hunk[4]);
      inHunk = remainingOld > 0 || remainingNew > 0;
      seenHunk = true;
      group.lines.push({ kind: "hunk", oldNo: null, newNo: null, text: line });
      continue;
    }

    if (inHunk) {
      const marker = line[0];
      if (marker === "+") {
        group.lines.push({
          kind: "add",
          oldNo: null,
          newNo: newCursor,
          text: line,
        });
        newCursor += 1;
        remainingNew -= 1;
        endHunkIfConsumed();
        continue;
      }
      if (marker === "-") {
        group.lines.push({
          kind: "del",
          oldNo: oldCursor,
          newNo: null,
          text: line,
        });
        oldCursor += 1;
        remainingOld -= 1;
        endHunkIfConsumed();
        continue;
      }
      if (marker === " ") {
        group.lines.push({
          kind: "context",
          oldNo: oldCursor,
          newNo: newCursor,
          text: line,
        });
        oldCursor += 1;
        newCursor += 1;
        remainingOld -= 1;
        remainingNew -= 1;
        endHunkIfConsumed();
        continue;
      }
      if (line.startsWith("\\")) {
        group.lines.push({
          kind: "meta",
          oldNo: null,
          newNo: null,
          text: line,
        });
        continue;
      }
    }

    if (!seenHunk && (line.startsWith("--- ") || line.startsWith("+++ "))) {
      group.lines.push({ kind: "meta", oldNo: null, newNo: null, text: line });
      continue;
    }

    if (line.startsWith("\\")) {
      group.lines.push({ kind: "meta", oldNo: null, newNo: null, text: line });
      continue;
    }

    group.lines.push({ kind: "meta", oldNo: null, newNo: null, text: line });
  }

  return groups.filter(
    (group) => group.lines.length > 0 || groups.length === 1,
  );
}

/** One hunk of a file's diff, as the view shows it. */
export type DiffHunk = {
  /** Zero-based position of the hunk in its file's diff. */
  index: number;
  /** Position of the hunk's `@@` line in the file group's lines. */
  line: number;
  /** The hunk as shown: its `@@` line through its last line, newline-joined. */
  text: string;
  oldStart: number;
  oldCount: number;
  newStart: number;
  newCount: number;
  /**
   * Every line the header promised is here. A diff cut at its size cap can
   * end partway through a hunk, and that hunk cannot be reverted as shown.
   */
  complete: boolean;
};

/**
 * What kind of change one file's section is, read from git's own header
 * lines, so a revert can say what reverting it does: an added file is
 * deleted, a renamed one goes back to its old name.
 */
export function fileChangeOf(group: DiffFileGroup): {
  kind: FileChangeKind;
  previousPath?: string;
} {
  let kind: FileChangeKind = "modified";
  let previousPath: string | undefined;
  for (const line of group.lines) {
    if (line.kind === "hunk") break;
    if (line.kind !== "meta") continue;
    if (line.text.startsWith("new file mode")) kind = "added";
    else if (line.text.startsWith("deleted file mode")) kind = "deleted";
    else if (line.text.startsWith("rename from ")) {
      kind = "renamed";
      previousPath = line.text.slice("rename from ".length);
    }
  }
  return previousPath === undefined ? { kind } : { kind, previousPath };
}

/**
 * Split one file's lines into its hunks.
 *
 * A hunk runs from its `@@` line to the line before the next one, which is
 * how the server splits the same file when it rebuilds a hunk to revert.
 */
export function diffHunks(group: DiffFileGroup): DiffHunk[] {
  const hunks: DiffHunk[] = [];
  let current: {
    line: number;
    texts: string[];
    oldStart: number;
    oldCount: number;
    newStart: number;
    newCount: number;
    oldSeen: number;
    newSeen: number;
  } | null = null;
  const finish = () => {
    if (!current) return;
    hunks.push({
      index: hunks.length,
      line: current.line,
      text: current.texts.join("\n"),
      oldStart: current.oldStart,
      oldCount: current.oldCount,
      newStart: current.newStart,
      newCount: current.newCount,
      complete:
        current.oldSeen === current.oldCount &&
        current.newSeen === current.newCount,
    });
  };
  group.lines.forEach((line, index) => {
    if (line.kind === "hunk") {
      finish();
      const header = HUNK_HEADER.exec(line.text);
      current = {
        line: index,
        texts: [line.text],
        oldStart: Number(header?.[1] ?? 0),
        oldCount: header?.[2] === undefined ? 1 : Number(header[2]),
        newStart: Number(header?.[3] ?? 0),
        newCount: header?.[4] === undefined ? 1 : Number(header[4]),
        oldSeen: 0,
        newSeen: 0,
      };
      return;
    }
    if (!current) return;
    current.texts.push(line.text);
    if (line.kind === "add" || line.kind === "context") current.newSeen += 1;
    if (line.kind === "del" || line.kind === "context") current.oldSeen += 1;
  });
  finish();
  return hunks;
}

export function parseDiffGitLine(line: string): string | null {
  if (!line.startsWith("diff --git ")) return null;
  const rest = line.slice("diff --git ".length);
  const paths = splitDiffGitPaths(rest);
  if (!paths) return null;
  return stripDiffPrefix(paths[1]) || stripDiffPrefix(paths[0]) || "file";
}

function splitDiffGitPaths(rest: string): [string, string] | null {
  const first = readDiffPath(rest, 0);
  if (!first) return null;
  if (rest[first.next] !== " ") return null;
  const second = readDiffPath(rest, first.next + 1);
  if (!second) return null;
  return [first.path, second.path];
}

function readDiffPath(
  source: string,
  start: number,
): { path: string; next: number } | null {
  if (start >= source.length) return null;
  if (source[start] === '"') {
    const quoted = unquoteCStyle(source.slice(start));
    if (!quoted) return null;
    return { path: quoted.value, next: start + quoted.consumed };
  }
  const space = source.indexOf(" ", start);
  const next = space === -1 ? source.length : space;
  return { path: source.slice(start, next), next };
}

function stripDiffPrefix(path: string): string {
  if (path.startsWith("a/") || path.startsWith("b/")) return path.slice(2);
  return path;
}

/**
 * Unquote a git C-style quoted path (`"foo\\tbar"` / octal bytes).
 * Returns null when the string is not a quoted token.
 */
export function unquoteCStyle(
  token: string,
): { value: string; consumed: number } | null {
  if (!token.startsWith('"')) return null;
  let i = 1;
  let out = "";
  while (i < token.length) {
    const ch = token[i];
    if (ch === '"') {
      return { value: decodeUtf8Binary(out), consumed: i + 1 };
    }
    if (ch !== "\\") {
      out += ch;
      i += 1;
      continue;
    }
    i += 1;
    if (i >= token.length) return null;
    const esc = token[i];
    if (esc === "n") {
      out += "\n";
      i += 1;
    } else if (esc === "t") {
      out += "\t";
      i += 1;
    } else if (esc === "r") {
      out += "\r";
      i += 1;
    } else if (esc === "\\" || esc === '"') {
      out += esc;
      i += 1;
    } else if (esc >= "0" && esc <= "7") {
      let oct = "";
      while (
        oct.length < 3 &&
        i < token.length &&
        token[i] >= "0" &&
        token[i] <= "7"
      ) {
        oct += token[i];
        i += 1;
      }
      out += String.fromCharCode(Number.parseInt(oct, 8));
    } else {
      out += esc;
      i += 1;
    }
  }
  return null;
}

function decodeUtf8Binary(binary: string): string {
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i) & 0xff;
  }
  return new TextDecoder().decode(bytes);
}
