// Fails when two tracked paths would collide on a case-insensitive disk.
//
// macOS checks the repository out onto a case-insensitive file system, where
// two paths that differ only in case name the same file. Imports have the same
// problem one step removed: `./DocumentTitle` resolved to documentTitle.ts,
// which sat next to DocumentTitle.tsx, and broke the macOS build while Linux
// CI passed. So this compares the entries of each directory without case, and
// compares JavaScript and TypeScript modules by the name an import uses, with
// the extension left off.
//
//   node scripts/check-path-case.mjs

import { execFileSync } from "node:child_process";
import { realpathSync } from "node:fs";
import { pathToFileURL } from "node:url";

// Extensions an import can leave off. Longest first, so `.d.ts` and `.mjs`
// match before `.ts` and `.js`.
const MODULE_EXTENSIONS = [
  ".d.cts",
  ".d.mts",
  ".d.ts",
  ".cjs",
  ".cts",
  ".jsx",
  ".mjs",
  ".mts",
  ".tsx",
  ".js",
  ".ts",
];

function importName(name) {
  const extension = MODULE_EXTENSIONS.find(
    (candidate) => name.endsWith(candidate) && name.length > candidate.length,
  );
  return extension ? name.slice(0, -extension.length) : name;
}

// Returns each group of paths that one case-insensitive name covers, sorted.
// A directory is compared with its siblings like a file, so `Docs/` and
// `docs/` collide once, at the directory, rather than once per file inside.
export function findCaseCollisions(paths) {
  const groups = new Map();
  for (const path of paths) {
    const parts = path.split("/");
    parts.forEach((name, depth) => {
      const parent = parts.slice(0, depth).join("/");
      const isFile = depth === parts.length - 1;
      const imported = isFile ? importName(name) : name;
      // APFS also treats Unicode normalization forms as the same name.
      const key = `${parent}\0${imported.normalize("NFC").toLowerCase()}`;
      if (!groups.has(key)) groups.set(key, new Map());
      groups.get(key).set(parent ? `${parent}/${name}` : name, imported);
    });
  }
  return [...groups.values()]
    .filter((group) => new Set(group.values()).size > 1)
    .map((group) => [...group.keys()].sort());
}

// Node reports this module by its real path, so resolve symlinks in argv
// before comparing; otherwise a symlinked checkout would skip the check.
if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href
) {
  const paths = execFileSync("git", ["ls-files", "-z"], {
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  })
    .split("\0")
    .filter(Boolean);
  const collisions = findCaseCollisions(paths);
  if (collisions.length > 0) {
    console.error(
      "These tracked paths collide on a case-insensitive file system such as macOS.",
    );
    console.error("Rename one path in each group so the names differ by more than case:");
    for (const group of collisions) {
      console.error(`\n  ${group.join("\n  ")}`);
    }
    process.exit(1);
  }
  console.log(`Checked ${paths.length} tracked paths; no two differ only in case.`);
}
