import assert from "node:assert/strict";
import test from "node:test";
import { findCaseCollisions } from "./check-path-case.mjs";

test("paths that differ only in case collide", () => {
  assert.deepEqual(findCaseCollisions(["README.md", "readme.md"]), [
    ["README.md", "readme.md"],
  ]);
  // APFS treats composed and decomposed accents as one name, too.
  assert.deepEqual(findCaseCollisions(["café.md", "café.md"]), [
    ["café.md", "café.md"],
  ]);
});

test("modules collide when their import names differ only in case", () => {
  // The macOS build break: `./DocumentTitle` resolved to the helper module.
  assert.deepEqual(
    findCaseCollisions(["ui/src/DocumentTitle.tsx", "ui/src/documentTitle.ts"]),
    [["ui/src/DocumentTitle.tsx", "ui/src/documentTitle.ts"]],
  );
  // An import of `./Panel` can also resolve to `Panel/index.ts`.
  assert.deepEqual(findCaseCollisions(["src/Panel/index.ts", "src/panel.ts"]), [
    ["src/Panel", "src/panel.ts"],
  ]);
});

test("a directory collides once, not once per file inside it", () => {
  assert.deepEqual(findCaseCollisions(["Docs/a.md", "docs/b.md", "docs/c.md"]), [
    ["Docs", "docs"],
  ]);
});

test("names that differ by more than case do not collide", () => {
  assert.deepEqual(
    findCaseCollisions([
      "README.md",
      "src/Foo.css",
      "src/foo.test.ts",
      "src/foo.ts",
      "src/foo.tsx",
      "src/foo/index.ts",
      "src/readme.md",
    ]),
    [],
  );
});
