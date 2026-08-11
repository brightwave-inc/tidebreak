#!/usr/bin/env node
// Behavior tests for the third-party notice pipeline. The graph collectors are
// exercised against real package directories on disk (synthesized in a
// temporary tree) rather than mocked filesystems, because the properties worth
// protecting are exactly the ones a mock would paper over: which packages are
// covered, what text survives, and whether the same inputs always render the
// same bytes.

import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  NOTICES_RELATIVE_PATH,
  REGENERATE_COMMAND,
  collectNodePackages,
  collectRustPackages,
  licenseTextId,
  normalizeLicenseText,
  parseSpdxIdentifiers,
  renderNotices,
} from "./generate-third-party-notices.mjs";

const repositoryRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
);

function repositoryFile(...segments) {
  return path.join(repositoryRoot, ...segments);
}

const APACHE_TEXT = "Apache License\nVersion 2.0\n";
const MIT_TEXT = 'MIT License\n\nCopyright (c) Example\n\nPermission is hereby granted `verbatim` ```fenced``` text\n';
const CC_TEXT = "Creative Commons Attribution 3.0\n";

function scratchTree() {
  const root = mkdtempSync(path.join(tmpdir(), "openwave-notices-"));
  test.after(() => rmSync(root, { recursive: true, force: true }));
  return root;
}

function writePackage(root, directory, files) {
  const packageDirectory = path.join(root, directory);
  for (const [name, contents] of Object.entries(files)) {
    const file = path.join(packageDirectory, name);
    mkdirSync(path.dirname(file), { recursive: true });
    writeFileSync(file, contents);
  }
  return packageDirectory;
}

test("the Rust collector covers the whole non-workspace graph and preserves its terms", () => {
  const root = scratchTree();

  const dual = writePackage(root, "dual", {
    "Cargo.toml": "",
    "LICENSE-APACHE": APACHE_TEXT,
    "LICENSE-MIT": MIT_TEXT,
    "src/lib.rs": "// not a license\n",
  });
  const compound = writePackage(root, "compound", {
    "Cargo.toml": "",
    "LICENSE-MIT": MIT_TEXT,
    "LICENSE-CC": CC_TEXT,
  });
  // Terms that exist only as a file, in a location no filename convention
  // would find, declared through the manifest.
  const fileOnly = writePackage(root, "file-only", {
    "Cargo.toml": "",
    "docs/terms.txt": "Bespoke terms.\n",
  });
  const bare = writePackage(root, "bare", { "Cargo.toml": "" });
  const member = writePackage(root, "member", {
    "Cargo.toml": "",
    LICENSE: "OpenWave's own license\n",
  });

  const packages = collectRustPackages({
    workspace_members: ["member 0.1.0 (path+file:///member)"],
    packages: [
      {
        id: "member 0.1.0 (path+file:///member)",
        name: "member",
        version: "0.1.0",
        license: "Apache-2.0",
        manifest_path: path.join(member, "Cargo.toml"),
      },
      {
        id: "dual",
        name: "dual",
        version: "1.2.3",
        license: "MIT OR Apache-2.0",
        repository: "https://example.invalid/dual",
        manifest_path: path.join(dual, "Cargo.toml"),
      },
      {
        id: "compound",
        name: "compound",
        version: "0.9.0",
        license: "(MIT AND CC-BY-3.0)",
        manifest_path: path.join(compound, "Cargo.toml"),
      },
      {
        id: "file-only",
        name: "file-only",
        version: "2.0.0",
        license: null,
        license_file: "docs/terms.txt",
        manifest_path: path.join(fileOnly, "Cargo.toml"),
      },
      {
        id: "bare",
        name: "bare",
        version: "0.1.0",
        license: "ISC",
        manifest_path: path.join(bare, "Cargo.toml"),
      },
    ],
  });

  assert.deepEqual(
    packages.map((entry) => `${entry.name} ${entry.version}`),
    ["bare 0.1.0", "compound 0.9.0", "dual 1.2.3", "file-only 2.0.0"],
    "workspace members are excluded and every other package is covered, sorted",
  );

  const byName = new Map(packages.map((entry) => [entry.name, entry]));

  // A compound expression is a fact about the package, not a choice for the
  // generator: it must survive verbatim, with every operand's text.
  assert.equal(byName.get("compound").license, "(MIT AND CC-BY-3.0)");
  assert.deepEqual(
    byName.get("compound").licenseTexts.map((entry) => entry.file),
    ["LICENSE-CC", "LICENSE-MIT"],
  );
  assert.deepEqual(parseSpdxIdentifiers("(MIT AND CC-BY-3.0)"), [
    "CC-BY-3.0",
    "MIT",
  ]);
  assert.deepEqual(parseSpdxIdentifiers("Apache-2.0 WITH LLVM-exception OR MIT"), [
    "Apache-2.0",
    "LLVM-exception",
    "MIT",
  ]);
  assert.deepEqual(parseSpdxIdentifiers("MIT/Apache-2.0"), ["Apache-2.0", "MIT"]);

  // A license-file-only package keeps its text and is reported as declaring no
  // SPDX expression rather than being dropped or guessed at.
  const declaredOnly = byName.get("file-only");
  assert.equal(declaredOnly.license, "not declared by the package");
  assert.deepEqual(declaredOnly.licenseTexts, [
    { file: "docs/terms.txt", text: "Bespoke terms." },
  ]);

  assert.deepEqual(byName.get("bare").licenseTexts, []);
  assert.deepEqual(
    byName.get("dual").licenseTexts.map((entry) => entry.file),
    ["LICENSE-APACHE", "LICENSE-MIT"],
    "source files are not mistaken for license text",
  );

  const rendered = renderNotices({ rustPackages: packages, nodePackages: [] });
  for (const text of [APACHE_TEXT, MIT_TEXT, CC_TEXT, "Bespoke terms."]) {
    assert.ok(
      rendered.includes(normalizeLicenseText(text)),
      "every collected license text is reproduced",
    );
  }
  // Shared text is stored once and referenced, or the file would be tens of
  // megabytes across a graph this size.
  const mitId = licenseTextId(normalizeLicenseText(MIT_TEXT));
  assert.equal(rendered.split(`### ${mitId}`).length - 1, 1);
  assert.equal(rendered.split(normalizeLicenseText(MIT_TEXT)).length - 1, 1);
  // Backticks inside license text must not escape their fence.
  assert.match(rendered, /````\nMIT License/);
  assert.match(rendered, /- License text: not distributed with this package/);
  assert.ok(rendered.includes("- `CC-BY-3.0`"), "summary names every identifier");
});

test("rendering is deterministic and independent of input order", () => {
  const root = scratchTree();
  const packages = ["zlib-rs", "aho-corasick", "serde"].map((name, index) => ({
    name,
    version: `${index}.1.0`,
    license: "MIT OR Apache-2.0",
    repository: null,
    licenseTexts: [{ file: "LICENSE", text: normalizeLicenseText(MIT_TEXT) }],
  }));
  // A second version of one package: ordering must break ties on version.
  packages.push({ ...packages[2], version: "0.0.9" });
  writePackage(root, "unused", { "Cargo.toml": "" });

  const forward = renderNotices({ rustPackages: packages, nodePackages: [] });
  const reversed = renderNotices({
    rustPackages: [...packages].reverse(),
    nodePackages: [],
  });
  assert.equal(forward, reversed);

  const headings = [...forward.matchAll(/^### (\S+) (\S+)$/gm)]
    .map((match) => `${match[1]} ${match[2]}`)
    .filter((heading) => !heading.startsWith("L-"));
  assert.deepEqual(headings, [
    "aho-corasick 1.1.0",
    "serde 0.0.9",
    "serde 2.1.0",
    "zlib-rs 0.1.0",
  ]);
  assert.ok(forward.endsWith("\n") && !forward.endsWith("\n\n"));
});

test("the UI collector reads terms from each package, not from pnpm's classification", () => {
  const root = scratchTree();

  const spdx = writePackage(root, "spdx", {
    "package.json": JSON.stringify({
      name: "spdx",
      license: "(MIT OR GPL-3.0-or-later)",
      repository: { url: "git+https://example.invalid/spdx.git" },
    }),
    LICENSE: MIT_TEXT,
  });
  // pnpm reports this bucket as "Unknown"; the package still distributes text,
  // and dropping either fact would understate what ships.
  const undeclared = writePackage(root, "undeclared", {
    "package.json": JSON.stringify({ name: "undeclared" }),
    "LICENSE.txt": APACHE_TEXT,
  });
  // Deprecated npm manifest shape, still published in the wild.
  const legacy = writePackage(root, "legacy", {
    "package.json": JSON.stringify({
      name: "legacy",
      licenses: [{ type: "MIT" }, { type: "Apache-2.0" }],
      repository: "https://example.invalid/legacy",
    }),
  });
  const ownProject = writePackage(root, "own", {
    "package.json": JSON.stringify({ name: "openwave-desktop-ui" }),
  });

  const packages = collectNodePackages(
    {
      "(MIT OR GPL-3.0-or-later)": [
        { name: "spdx", versions: ["1.0.0"], paths: [spdx] },
      ],
      Unknown: [
        { name: "undeclared", versions: ["0.25.1"], paths: [undeclared] },
        {
          name: "openwave-desktop-ui",
          versions: ["0.0.0"],
          paths: [ownProject],
        },
      ],
      MIT: [{ name: "legacy", versions: ["0.1.0"], paths: [legacy] }],
    },
    { excludeNames: ["openwave-desktop-ui"] },
  );

  assert.deepEqual(
    packages.map((entry) => [entry.name, entry.license]),
    [
      ["legacy", "MIT OR Apache-2.0"],
      ["spdx", "(MIT OR GPL-3.0-or-later)"],
      ["undeclared", "not declared by the package"],
    ],
  );
  assert.deepEqual(
    packages.find((entry) => entry.name === "undeclared").licenseTexts,
    [{ file: "LICENSE.txt", text: normalizeLicenseText(APACHE_TEXT) }],
  );
  assert.equal(
    packages.find((entry) => entry.name === "spdx").repository,
    "git+https://example.invalid/spdx.git",
  );
});

test("a graph the checkout cannot back with files fails instead of shrinking", () => {
  const root = scratchTree();
  assert.throws(
    () =>
      collectRustPackages({
        workspace_members: [],
        packages: [
          {
            id: "absent",
            name: "absent",
            version: "1.0.0",
            license: "MIT",
            manifest_path: path.join(root, "absent", "Cargo.toml"),
          },
        ],
      }),
    /no unpacked source for absent 1\.0\.0/,
  );
  assert.throws(
    () =>
      collectNodePackages({
        MIT: [
          {
            name: "absent",
            versions: ["1.0.0"],
            paths: [path.join(root, "absent")],
          },
        ],
      }),
    /no installed package for absent 1\.0\.0/,
  );
  assert.throws(
    () =>
      collectNodePackages({
        MIT: [{ name: "absent", versions: ["1.0.0", "2.0.0"], paths: [root] }],
      }),
    /pnpm reported 2 versions and 1 paths/,
  );
});

test("the notices ship with the desktop app and are verified against drift", () => {
  const notices = readFileSync(repositoryFile(NOTICES_RELATIVE_PATH), "utf8");
  assert.ok(notices.startsWith("# Third-party notices\n"));
  assert.ok(
    notices.includes(REGENERATE_COMMAND),
    "the generated file must say how to regenerate it",
  );
  assert.match(notices, /^## License texts$/m);

  // The bundle resource map is what puts the file inside the .app, and every
  // macOS artifact (DMG, updater archive, zip) is derived from that bundle.
  const tauriConfig = JSON.parse(
    readFileSync(
      repositoryFile("crates", "openwave-desktop", "tauri.conf.json"),
      "utf8",
    ),
  );
  assert.equal(
    tauriConfig.bundle.resources[`../../${NOTICES_RELATIVE_PATH}`],
    NOTICES_RELATIVE_PATH,
  );

  // Bundling is only load-bearing if the release lane refuses a bundle without
  // it, and CI refuses a checked-in file that no longer matches the graph.
  const release = readFileSync(
    repositoryFile(".github", "workflows", "release.yml"),
    "utf8",
  );
  assert.ok(
    release.includes(`${NOTICES_RELATIVE_PATH}:${NOTICES_RELATIVE_PATH}`) &&
      release.includes('cmp "$source_file" "$bundled"') &&
      release.includes('"$app_path/Contents/Resources/$bundled_path"'),
    "the release lane must compare the bundled notices against the checked-in file",
  );
  assert.ok(
    release.includes(`${REGENERATE_COMMAND} --check`),
    "the release lane must refuse a tag whose notices are stale",
  );
  const ci = readFileSync(
    repositoryFile(".github", "workflows", "ci.yml"),
    "utf8",
  );
  assert.ok(ci.includes(`${REGENERATE_COMMAND} --check`));
});
