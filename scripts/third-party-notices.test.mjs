#!/usr/bin/env node
// Behavior tests for the third-party notice pipeline. The graph collectors are
// exercised against real package directories on disk (synthesized in a
// temporary tree) rather than mocked filesystems, because the properties worth
// protecting are exactly the ones a mock would paper over: which packages are
// covered, what text survives, and whether the same inputs always render the
// same bytes.

import assert from "node:assert/strict";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  NOTICES_RELATIVE_PATH,
  REGENERATE_COMMAND,
  collectNodePackages,
  collectRustPackages,
  firstDifference,
  installedPackages,
  licenseTextId,
  normalizeLicenseText,
  normalizeNoticesForComparison,
  parseSpdxIdentifiers,
  pnpmInvocation,
  productionClosureConfig,
  renderNotices,
  SUPPORTED_ARCHITECTURES,
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
  const root = mkdtempSync(path.join(tmpdir(), "tidebreak-notices-"));
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

test("the notices generator resolves the Windows pnpm command shim", () => {
  const args = ["install", "--prod", "--frozen-lockfile", "--ignore-scripts"];
  assert.deepEqual(
    pnpmInvocation(args, "win32", "C:\\Windows\\System32\\cmd.exe"),
    {
      executable: "C:\\Windows\\System32\\cmd.exe",
      args: ["/d", "/c", "pnpm", ...args],
    },
  );
  assert.deepEqual(pnpmInvocation(args, "linux"), {
    executable: "pnpm",
    args,
  });
});

test("the UI closure is resolved for every platform, never the generating host", () => {
  // A package with one native build per platform must appear in full on every
  // host, so the install must name its platforms outright. `current` would
  // reintroduce the host into the output.
  for (const values of Object.values(SUPPORTED_ARCHITECTURES)) {
    assert.ok(values.length > 1);
    assert.ok(!values.includes("current"));
  }
  for (const [os, cpu] of [
    ["linux", "x64"],
    ["linux", "arm64"],
    ["darwin", "arm64"],
    ["win32", "x64"],
  ]) {
    assert.ok(SUPPORTED_ARCHITECTURES.os.includes(os));
    assert.ok(SUPPORTED_ARCHITECTURES.cpu.includes(cpu));
  }

  const config = productionClosureConfig(
    "overrides:\n  left-pad@1.0.0: 1.3.0\n\n",
  );
  assert.equal(
    config,
    "overrides:\n  left-pad@1.0.0: 1.3.0\n" +
      "supportedArchitectures:\n" +
      `  os: [${SUPPORTED_ARCHITECTURES.os.join(", ")}]\n` +
      `  cpu: [${SUPPORTED_ARCHITECTURES.cpu.join(", ")}]\n` +
      "  libc: [glibc, musl]\n",
    "the UI's own settings, overrides above all, survive ahead of the platforms",
  );
  assert.match(productionClosureConfig(""), /^supportedArchitectures:\n/);
  assert.throws(
    () => productionClosureConfig("supportedArchitectures:\n  os: [current]\n"),
    /already sets supportedArchitectures/,
  );
});

test("the notices check ignores Git's Windows line-ending conversion", () => {
  const notices = "# Third-party notices\n\nGenerated terms.\n";
  assert.equal(
    normalizeNoticesForComparison(notices.replaceAll("\n", "\r\n")),
    normalizeNoticesForComparison(notices),
  );

  const staleWindowsNotices = notices
    .replace("Generated terms.", "Stale terms.")
    .replaceAll("\n", "\r\n");
  assert.deepEqual(
    firstDifference(
      normalizeNoticesForComparison(staleWindowsNotices),
      normalizeNoticesForComparison(notices),
    ),
    { line: 3, expected: "Stale terms.", actual: "Generated terms." },
  );
});

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
    LICENSE: "Tidebreak's own license\n",
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
    "package.json": JSON.stringify({ name: "tidebreak-desktop-ui" }),
  });

  const packages = collectNodePackages(
    [
      { name: "spdx", version: "1.0.0", path: spdx },
      { name: "undeclared", version: "0.25.1", path: undeclared },
      { name: "tidebreak-desktop-ui", version: "0.0.0", path: ownProject },
      { name: "legacy", version: "0.1.0", path: legacy },
    ],
    { excludeNames: ["tidebreak-desktop-ui"] },
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
      collectNodePackages([
        { name: "absent", version: "1.0.0", path: path.join(root, "absent") },
      ]),
    /no installed package for absent 1\.0\.0/,
  );
  assert.throws(
    () => installedPackages(path.join(root, "never-installed")),
    /no pnpm virtual store/,
  );
});

test("the installed closure is read from pnpm's virtual store, every platform included", () => {
  const root = scratchTree();
  const store = path.join(root, "node_modules", ".pnpm");
  const instance = (key, name, manifest) =>
    writePackage(root, path.join("node_modules", ".pnpm", key, "node_modules", name), {
      "package.json": JSON.stringify({ name, ...manifest }),
    });

  const vue = instance("vue@3.5.0_typescript@7.0.0", "vue", { version: "3.5.0" });
  // The same package resolved with different peers is two instances of
  // identical files, and one package to the notices.
  instance("vue@3.5.0_typescript@7.0.0_zod@4.0.0", "vue", { version: "3.5.0" });
  // Both native builds sit in the store, however the host is built. Scoped
  // names nest one level deeper.
  const linux = instance(
    "@typescript+typescript-linux-x64@7.0.0",
    "@typescript/typescript-linux-x64",
    { version: "7.0.0", os: ["linux"], cpu: ["x64"] },
  );
  instance(
    "@typescript+typescript-darwin-arm64@7.0.0",
    "@typescript/typescript-darwin-arm64",
    { version: "7.0.0", os: ["darwin"], cpu: ["arm64"] },
  );
  // A dependency beside a package is a symlink into another instance; only
  // the real directory is that instance's package.
  symlinkSync(linux, path.join(path.dirname(vue), "typescript-link"), "dir");
  // pnpm's hoisted symlink directory and its lockfile copy are not packages.
  mkdirSync(path.join(store, "node_modules"), { recursive: true });
  writeFileSync(path.join(store, "lock.yaml"), "lockfileVersion: '9.0'\n");

  assert.deepEqual(
    installedPackages(path.join(root, "node_modules")).map(
      (entry) => `${entry.name}@${entry.version}`,
    ),
    [
      "@typescript/typescript-darwin-arm64@7.0.0",
      "@typescript/typescript-linux-x64@7.0.0",
      "vue@3.5.0",
    ],
  );
});

test("a curated license applies only while the evidence behind it holds", () => {
  const root = scratchTree();

  const univer = writePackage(root, "univer-pro", {
    "package.json": JSON.stringify({
      name: "@univerjs-pro/engine-formula",
      repository: { type: "git", url: "git+https://github.com/dream-num/univer.git" },
    }),
  });

  const graph = (packageDirectory) => [
    {
      name: JSON.parse(
        readFileSync(path.join(packageDirectory, "package.json"), "utf8"),
      ).name,
      version: "0.25.1",
      path: packageDirectory,
    },
  ];

  const [curated] = collectNodePackages(graph(univer));
  assert.equal(curated.license, "Apache-2.0");
  assert.match(curated.licenseNote, /manifest declares no license/);
  // The asserted terms travel with the entry, deduped against every other
  // Apache-2.0 text by content like any distributed one.
  assert.equal(curated.licenseTexts.length, 1);
  assert.match(curated.licenseTexts[0].text, /^ *Apache License\n/);
  assert.equal(
    licenseTextId(curated.licenseTexts[0].text),
    licenseTextId(
      normalizeLicenseText(
        readFileSync(
          repositoryFile("scripts", "license-texts", "apache-2.0.txt"),
          "utf8",
        ),
      ),
    ),
  );

  // The two facts the override rests on. Either changing means the review that
  // produced it no longer covers what ships, so the run must fail rather than
  // apply it — in particular an override must never overrule a declaration.
  const declaring = writePackage(root, "declaring", {
    "package.json": JSON.stringify({
      name: "@univerjs-pro/engine-formula",
      license: "MIT",
      repository: "https://github.com/dream-num/univer",
    }),
  });
  assert.throws(
    () => collectNodePackages(graph(declaring)),
    /now declares `MIT`/,
  );

  const moved = writePackage(root, "moved", {
    "package.json": JSON.stringify({
      name: "@univerjs/telemetry",
      repository: "https://github.com/someone-else/univer",
    }),
  });
  assert.throws(
    () => collectNodePackages(graph(moved)),
    /no longer points at github\.com\/dream-num\/univer/,
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
      repositoryFile("crates", "tidebreak-desktop", "tauri.conf.json"),
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
