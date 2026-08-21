import { describe, expect, it } from "vitest";

import {
  ancestorPaths,
  buildFileTree,
  filterPaths,
  matchGlob,
  treeIndentPx,
} from "./fileTree";

describe("buildFileTree", () => {
  it("nests directories and sorts folders before files", () => {
    const tree = buildFileTree([
      "README.md",
      "src/lib.rs",
      "src/code/mod.rs",
      "Cargo.toml",
    ]);
    expect(tree.map((node) => node.name)).toEqual([
      "src",
      "Cargo.toml",
      "README.md",
    ]);
    expect(tree[0]).toMatchObject({ kind: "dir", path: "src" });
    expect(tree[0].children?.map((node) => node.name)).toEqual([
      "code",
      "lib.rs",
    ]);
    expect(tree[0].children?.[0].children?.[0]).toEqual({
      kind: "file",
      name: "mod.rs",
      path: "src/code/mod.rs",
    });
  });
});

describe("matchGlob", () => {
  it("matches a basename pattern against any depth", () => {
    expect(matchGlob("src/lib.rs", "*.rs")).toBe(true);
    expect(matchGlob("README.md", "*.rs")).toBe(false);
    expect(matchGlob("src/lib.rs", "src/**")).toBe(true);
    expect(matchGlob("Cargo.toml", "src/**")).toBe(false);
  });
});

describe("filterPaths", () => {
  const paths = ["README.md", "src/lib.rs", "src/code/mod.rs", "Cargo.lock"];

  it("keeps include hits and drops exclude hits", () => {
    expect(filterPaths(paths, "*.rs", "")).toEqual([
      "src/lib.rs",
      "src/code/mod.rs",
    ]);
    expect(filterPaths(paths, "", "*.lock, *.md")).toEqual([
      "src/lib.rs",
      "src/code/mod.rs",
    ]);
  });
});

describe("ancestorPaths", () => {
  it("lists parent directories of a file", () => {
    expect(ancestorPaths("src/code/mod.rs")).toEqual(["src", "src/code"]);
    expect(ancestorPaths("README.md")).toEqual([]);
  });
});

describe("treeIndentPx", () => {
  it("stops growing so a deep path still leaves room for the name", () => {
    expect(treeIndentPx(0)).toBe(8);
    expect(treeIndentPx(3)).toBe(44);
    // Twenty levels deep would otherwise indent past the width of the rail.
    expect(treeIndentPx(20)).toBe(treeIndentPx(8));
    expect(treeIndentPx(8)).toBeLessThan(120);
  });
});
