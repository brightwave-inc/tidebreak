#!/usr/bin/env node
// Generate legal/THIRD-PARTY-NOTICES.md from the resolved dependency graphs the
// product actually ships: the Cargo workspace's non-member packages and the
// desktop UI's production npm closure.
//
// The generated file is checked in and bundled into the desktop app, so the
// only property that matters as much as completeness is determinism: the same
// lockfiles must always produce byte-identical output. Three rules keep that
// true. Nothing derived from the host — absolute paths, timestamps, package
// counts of the local checkout — reaches the output; both graphs are resolved
// for every platform the lockfiles can ship to, not the one generating the
// file; and anything that could silently reduce coverage (an unpacked source
// tree that is missing, a package manager that reports no graph) is a hard
// error rather than a smaller file.
//
// Dependency-light on purpose: `cargo metadata` and `pnpm install` are
// already required to build the product, and both are invoked as pure graph
// oracles. License facts are read from each package's own vendored files and
// manifest, never from a tool's classification, so upgrading either tool
// cannot rewrite the notices.

import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import {
  copyFileSync,
  existsSync,
  lstatSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  realpathSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const repositoryRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
);

export const NOTICES_RELATIVE_PATH = "legal/THIRD-PARTY-NOTICES.md";
export const UI_RELATIVE_PATH = "crates/tidebreak-desktop/ui";
export const REGENERATE_COMMAND = "node scripts/generate-third-party-notices.mjs";
// Cargo workspaces the root one excludes but Tidebreak still distributes. The
// whisper.cpp helper is published from its own workspace so that no
// `--workspace` lane compiles it; its dependency graph still ships to users.
export const EXCLUDED_CARGO_WORKSPACES = ["crates/tidebreak-whisper"];

// Files a package may distribute its license or notice text in. Matched
// case-insensitively against the top level of the package directory only:
// deeper trees hold source, and a package that keeps its terms somewhere else
// declares that path in its manifest, which is read separately.
const LICENSE_FILE_PATTERN =
  /^(licen[sc]es?|copying|copyright|notice|unlicen[sc]e|patents)(?![a-z])/i;

// Text with no letters carries no terms. A package that ships an empty or
// placeholder license file is recorded as distributing none, which is both
// more honest and stable across checkouts than an empty quotation.
const MEANINGFUL_TEXT_PATTERN = /[A-Za-z]/;

const UNDECLARED_LICENSE = "not declared by the package";

// pnpm installs only the optional dependencies whose `os`, `cpu`, and `libc`
// match the host. A package that publishes one native build per platform
// (TypeScript 7's compiler, for example) would therefore make the notices
// depend on where they were generated: each host sees its own variant and
// none of the others, and a file regenerated on Linux fails the check on macOS
// and Windows.
//
// So the production closure is resolved in a scratch copy of the UI project
// that pnpm is told to install for every platform, the same way the Cargo
// graph is read with `--all-features` to cover other targets. The lists are
// explicit rather than `current` so the closure is the same on every host;
// they are the platform and architecture names npm packages declare, which are
// Node's `process.platform` and `process.arch` values plus the WebAssembly
// pseudo-architecture. Production only: development dependencies are not
// distributed, and their native variants would add gigabytes for nothing.
//
// The installed tree is then read directly rather than through
// `pnpm licenses list`, which pnpm 10 still filters to the host's platform
// even when other platforms' packages are installed (pnpm 11 reports them
// all). The virtual store under `node_modules/.pnpm` holds exactly one
// directory per installed package instance, so it is the graph.
export const SUPPORTED_ARCHITECTURES = {
  os: [
    "aix",
    "android",
    "cygwin",
    "darwin",
    "freebsd",
    "linux",
    "netbsd",
    "openbsd",
    "openharmony",
    "sunos",
    "win32",
  ],
  cpu: [
    "arm",
    "arm64",
    "ia32",
    "loong64",
    "mips",
    "mipsel",
    "mips64el",
    "ppc",
    "ppc64",
    "riscv64",
    "s390",
    "s390x",
    "wasm32",
    "x64",
  ],
  libc: ["glibc", "musl"],
};

// Files pnpm reads when resolving and installing the UI project, and so the
// files that decide the production closure. `overrides` live in
// pnpm-workspace.yaml, which is why it must travel with the lockfile.
const UI_CLOSURE_FILES = ["package.json", "pnpm-lock.yaml"];
const UI_CLOSURE_OPTIONAL_FILES = ["pnpm-workspace.yaml", ".npmrc", ".pnpmfile.cjs"];

// Curated license facts, for packages whose terms are established somewhere
// other than their own manifest.
//
// Recording such a package as undeclared understates what the product ships,
// but asserting a license from a hand-maintained table is only defensible while
// the evidence behind it still holds. So an override names its evidence, and
// the generator re-checks that evidence on every run: the package must still
// declare no license of its own, and its repository must still point where the
// review looked. Either check failing is a hard error, not a silent fallback —
// a package that starts declaring its own license, or whose repository moves,
// needs a human to look again rather than an old assertion applied to new
// facts. An override can therefore never overrule a declaration.
//
// The license text is read from a file in this repository rather than fetched,
// so the output stays reproducible offline; it is normalized and
// content-addressed like any other license text, and shares the appendix entry
// with every package that distributes the same bytes.
const CURATED_APACHE_2_0_TEXT_FILE = "scripts/license-texts/apache-2.0.txt";

const CURATED_NODE_LICENSES = [
  {
    // The Univer project publishes these packages. Their npm manifests carry no
    // `license` field, and their `repository` field points at
    // github.com/dream-num/univer, which is Apache-2.0
    // (https://github.com/dream-num/univer/blob/dev/LICENSE). Recorded as
    // Apache-2.0 by maintainer decision, 2026-08-11.
    applies: (name) =>
      name.startsWith("@univerjs-pro/") || name === "@univerjs/telemetry",
    repository: "github.com/dream-num/univer",
    license: "Apache-2.0",
    licenseTextFile: CURATED_APACHE_2_0_TEXT_FILE,
    note:
      "curated — the manifest declares no license; the package's repository is " +
      "github.com/dream-num/univer, which is Apache-2.0 " +
      "(maintainer decision, 2026-08-11)",
    licenseTextLabel: "Apache-2.0 (curated, not distributed by the package)",
  },
];

function compareStrings(left, right) {
  // Code-unit ordering. `localeCompare` is locale- and ICU-dependent, which
  // would make the generated ordering vary between machines.
  if (left < right) return -1;
  if (left > right) return 1;
  return 0;
}

function comparePackages(left, right) {
  return (
    compareStrings(left.name, right.name) ||
    compareStrings(left.version, right.version)
  );
}

export function normalizeLicenseText(raw) {
  return raw
    .replace(/^﻿/, "")
    .replace(/\r\n?/g, "\n")
    // Trailing whitespace carries no terms and is not reproducible: it varies
    // with how a package was packed, and Git's own whitespace check rejects it.
    .replace(/[ \t]+$/gm, "")
    .replace(/\s+$/, "");
}

export function normalizeNoticesForComparison(raw) {
  // Git may materialize text files with CRLF on Windows. The generated notices
  // deliberately use LF for deterministic repository bytes, but a check of an
  // otherwise identical checkout must not fail solely because of Git's local
  // line-ending conversion.
  return raw.replace(/\r\n?/g, "\n");
}

export function licenseTextId(normalizedText) {
  const digest = createHash("sha256").update(normalizedText, "utf8").digest("hex");
  return `L-${digest.slice(0, 12)}`;
}

// Reduce an SPDX expression to the identifiers it names. Compound expressions
// are reported whole in the notices; this list only exists so the summary can
// enumerate every license the graph relies on, including the operands of an
// `OR` we never had to choose between.
export function parseSpdxIdentifiers(expression) {
  if (!expression) return [];
  const identifiers = new Set();
  for (const token of expression.split(/[()\s/]+|\bAND\b|\bOR\b|\bWITH\b/)) {
    const trimmed = token.trim();
    if (!trimmed) continue;
    if (/^(AND|OR|WITH)$/i.test(trimmed)) continue;
    identifiers.add(trimmed);
  }
  return [...identifiers].sort(compareStrings);
}

function readTextFile(file) {
  const normalized = normalizeLicenseText(readFileSync(file, "utf8"));
  return MEANINGFUL_TEXT_PATTERN.test(normalized) ? normalized : null;
}

function isFile(file) {
  try {
    return statSync(file).isFile();
  } catch {
    return false;
  }
}

// Collect the license and notice text a package distributes. `declaredFile` is
// the manifest-declared path used by packages whose terms live outside the
// conventional filenames; it is included even when it does not match them.
function collectLicenseTexts(packageDirectory, declaredFile) {
  const named = new Map();
  for (const entry of readdirSync(packageDirectory, { withFileTypes: true })) {
    if (!entry.isFile()) continue;
    if (!LICENSE_FILE_PATTERN.test(entry.name)) continue;
    named.set(entry.name, path.join(packageDirectory, entry.name));
  }
  if (declaredFile) {
    const resolved = path.resolve(packageDirectory, declaredFile);
    const relative = path.relative(packageDirectory, resolved);
    if (
      !relative.startsWith("..") &&
      !path.isAbsolute(relative) &&
      isFile(resolved)
    ) {
      named.set(relative.split(path.sep).join("/"), resolved);
    }
  }

  const texts = [];
  for (const name of [...named.keys()].sort(compareStrings)) {
    const text = readTextFile(named.get(name));
    if (text) texts.push({ file: name, text });
  }
  return texts;
}

export function collectRustPackages(metadata) {
  const members = new Set(metadata.workspace_members ?? []);
  const packages = [];
  for (const pkg of metadata.packages ?? []) {
    if (members.has(pkg.id)) continue;
    const packageDirectory = path.dirname(pkg.manifest_path);
    if (!existsSync(packageDirectory)) {
      // Absence here would quietly drop a package's terms from the notices and
      // make the output depend on what this checkout happens to have unpacked.
      throw new Error(
        `no unpacked source for ${pkg.name} ${pkg.version} at ${packageDirectory}; ` +
          "run `cargo fetch --locked` and regenerate",
      );
    }
    packages.push({
      name: pkg.name,
      version: pkg.version,
      license: pkg.license || UNDECLARED_LICENSE,
      repository: typeof pkg.repository === "string" ? pkg.repository : null,
      licenseTexts: collectLicenseTexts(packageDirectory, pkg.license_file),
    });
  }
  return packages.sort(comparePackages);
}

function manifestLicense(manifest) {
  if (typeof manifest.license === "string" && manifest.license.trim()) {
    return manifest.license.trim();
  }
  // Deprecated npm shapes still in the wild: `license: {type}` and the
  // `licenses` array. Dropping them would report a licensed package as
  // undeclared.
  if (manifest.license && typeof manifest.license.type === "string") {
    return manifest.license.type.trim();
  }
  if (Array.isArray(manifest.licenses)) {
    const types = manifest.licenses
      .map((entry) =>
        typeof entry === "string" ? entry : (entry?.type ?? ""),
      )
      .map((type) => type.trim())
      .filter(Boolean);
    if (types.length > 0) return types.join(" OR ");
  }
  return null;
}

function manifestRepository(manifest) {
  const repository = manifest.repository;
  if (typeof repository === "string") return repository;
  if (repository && typeof repository.url === "string") return repository.url;
  return null;
}

// Reduce a repository URL to `host/path` so the same repository written as an
// HTTPS URL, an `scp`-style remote, or with a `git+` prefix and a `.git` suffix
// all compare equal.
function normalizeRepositoryUrl(raw) {
  return raw
    .trim()
    .toLowerCase()
    .replace(/^git\+/, "")
    .replace(/^(?:https?|ssh|git):\/\//, "")
    .replace(/^[^@/]+@/, "")
    .replace(/^([^/]+):/, "$1/")
    .replace(/\.git$/, "")
    .replace(/\/+$/, "");
}

function pointsAtRepository(repository, expected) {
  if (typeof repository !== "string") return false;
  const normalized = normalizeRepositoryUrl(repository);
  return normalized === expected || normalized.startsWith(`${expected}/`);
}

const curatedLicenseTexts = new Map();

function curatedLicenseText(root, relativeFile) {
  const file = path.join(root, relativeFile);
  let text = curatedLicenseTexts.get(file);
  if (text === undefined) {
    if (!isFile(file)) {
      throw new Error(`missing curated license text ${relativeFile}`);
    }
    text = readTextFile(file);
    if (!text) {
      throw new Error(`curated license text ${relativeFile} carries no terms`);
    }
    curatedLicenseTexts.set(file, text);
  }
  return text;
}

// Apply a curated override to one package, after re-checking the evidence it
// rests on. Returns null when no override covers the package.
function curatedNodeLicense(
  { name, declaredLicense, repository },
  { root = repositoryRoot } = {},
) {
  const rule = CURATED_NODE_LICENSES.find((entry) => entry.applies(name));
  if (!rule) return null;
  if (declaredLicense) {
    throw new Error(
      `${name} now declares \`${declaredLicense}\`, so the curated ` +
        `${rule.license} override no longer applies; drop or narrow the rule ` +
        "in CURATED_NODE_LICENSES after re-reviewing the package",
    );
  }
  if (!pointsAtRepository(repository, rule.repository)) {
    throw new Error(
      `${name} no longer points at ${rule.repository} (repository: ` +
        `${repository ?? "none"}), so the evidence behind its curated ` +
        `${rule.license} override is stale; re-review the package and update ` +
        "CURATED_NODE_LICENSES",
    );
  }
  return {
    license: rule.license,
    note: rule.note,
    licenseTexts: [
      {
        file: rule.licenseTextLabel,
        text: curatedLicenseText(root, rule.licenseTextFile),
      },
    ],
  };
}

// `installed` is the list of `{name, version, path}` the scratch install put
// on disk. pnpm's own license classification is deliberately not consulted:
// it reads the manifest we read ourselves, and its heuristics are free to
// change between pnpm releases.
export function collectNodePackages(
  installed,
  { excludeNames = [], root = repositoryRoot } = {},
) {
  const excluded = new Set(excludeNames);
  const packages = new Map();
  for (const { name, version, path: packageDirectory } of installed) {
    if (excluded.has(name)) continue;
    const manifestPath = path.join(packageDirectory, "package.json");
    if (!isFile(manifestPath)) {
      throw new Error(
        `no installed package for ${name} ${version} at ${packageDirectory}`,
      );
    }
    const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    const declaredLicense = manifestLicense(manifest);
    const repository = manifestRepository(manifest);
    const distributedTexts = collectLicenseTexts(packageDirectory, null);
    const curated = curatedNodeLicense(
      { name, declaredLicense, repository },
      { root },
    );
    packages.set(`${name}@${version}`, {
      name,
      version,
      license: curated?.license ?? declaredLicense ?? UNDECLARED_LICENSE,
      licenseNote: curated?.note ?? null,
      repository,
      licenseTexts: [...distributedTexts, ...(curated?.licenseTexts ?? [])],
    });
  }
  return [...packages.values()].sort(comparePackages);
}

// Fence license text with a run of backticks longer than any it contains, so
// verbatim text is never reinterpreted as markdown and never truncated.
function fenceFor(text) {
  let longest = 0;
  for (const run of text.match(/`+/g) ?? []) {
    longest = Math.max(longest, run.length);
  }
  return "`".repeat(Math.max(3, longest + 1));
}

function renderSection(heading, packages, texts) {
  const lines = [`## ${heading}`, ""];
  if (packages.length === 0) {
    lines.push("None.", "");
    return lines;
  }
  for (const pkg of packages) {
    lines.push(`### ${pkg.name} ${pkg.version}`, "");
    lines.push(`- License: \`${pkg.license}\``);
    if (pkg.licenseNote) lines.push(`- License source: ${pkg.licenseNote}`);
    if (pkg.repository) lines.push(`- Repository: ${pkg.repository}`);
    if (pkg.licenseTexts.length === 0) {
      lines.push("- License text: not distributed with this package");
    } else {
      const references = pkg.licenseTexts.map((entry) => {
        const id = licenseTextId(entry.text);
        const existing = texts.get(id);
        if (existing !== undefined && existing !== entry.text) {
          // Truncated digests are shared identity; a collision would drop one
          // package's terms in favour of another's.
          throw new Error(`license text identifier collision on ${id}`);
        }
        texts.set(id, entry.text);
        return `\`${entry.file}\` ([${id}](#${id.toLowerCase()}))`;
      });
      lines.push(`- License text: ${references.join(", ")}`);
    }
    lines.push("");
  }
  return lines;
}

export function renderNotices({ rustPackages, nodePackages }) {
  const sortedRust = [...rustPackages].sort(comparePackages);
  const sortedNode = [...nodePackages].sort(comparePackages);
  const texts = new Map();

  const body = [
    ...renderSection("Rust crates", sortedRust, texts),
    ...renderSection("Desktop UI production packages", sortedNode, texts),
  ];

  const identifiers = new Set();
  for (const pkg of [...sortedRust, ...sortedNode]) {
    if (pkg.license === UNDECLARED_LICENSE) continue;
    for (const identifier of parseSpdxIdentifiers(pkg.license)) {
      identifiers.add(identifier);
    }
  }
  const undeclared = [...sortedRust, ...sortedNode].filter(
    (pkg) => pkg.license === UNDECLARED_LICENSE,
  );
  const curated = [...sortedRust, ...sortedNode].filter((pkg) => pkg.licenseNote);

  const header = [
    "# Third-party notices",
    "",
    "Tidebreak is distributed with the third-party software listed below. This",
    "file is generated; do not edit it by hand. Regenerate it with:",
    "",
    "```",
    REGENERATE_COMMAND,
    "```",
    "",
    "It covers every package in the resolved Cargo workspace graphs (the root",
    "workspace and the separately published whisper helper) that is not a",
    "Tidebreak crate, and every package in the desktop UI's production",
    "dependency graph, on every platform either graph can be built for.",
    "Development-only dependencies are excluded because they are not",
    "distributed.",
    "",
    "Each entry records the license expression the package declares, verbatim.",
    "A compound expression is reproduced as written rather than resolved to one",
    "of its operands, and every license or notice file the package distributes",
    "is reproduced under [License texts](#license-texts). Identical texts are",
    "stored once and referenced by a content-addressed identifier.",
    "",
    "A few packages state their terms outside their own manifest. Those carry a",
    "`License source` line naming the evidence behind the recorded license; the",
    "generator re-checks that evidence on every run and fails rather than apply",
    "a stale one.",
    "",
    "## Summary",
    "",
    `- Rust crates: ${sortedRust.length}`,
    `- Desktop UI production packages: ${sortedNode.length}`,
    `- Distinct license texts: ${texts.size}`,
    `- Packages with no declared license: ${undeclared.length}`,
    `- Packages with a curated license: ${curated.length}`,
    "",
    "License identifiers named across all declared expressions:",
    "",
    ...[...identifiers]
      .sort(compareStrings)
      .map((identifier) => `- \`${identifier}\``),
    "",
  ];

  const appendix = ["## License texts", ""];
  for (const id of [...texts.keys()].sort(compareStrings)) {
    const text = texts.get(id);
    const fence = fenceFor(text);
    appendix.push(`### ${id}`, "", fence, text, fence, "");
  }

  return `${[...header, ...body, ...appendix].join("\n").replace(/\n+$/, "")}\n`;
}

// Combine the metadata of several Cargo workspaces into one graph: every
// workspace's own members are excluded and a package that appears in more
// than one graph is listed once.
export function mergeCargoMetadata(metadatas) {
  const workspaceMembers = new Set();
  const packages = new Map();
  for (const metadata of metadatas) {
    for (const member of metadata.workspace_members ?? []) {
      workspaceMembers.add(member);
    }
    for (const pkg of metadata.packages ?? []) {
      if (!packages.has(pkg.id)) packages.set(pkg.id, pkg);
    }
  }
  return {
    workspace_members: [...workspaceMembers],
    packages: [...packages.values()],
  };
}

function runCargoMetadata(root) {
  const output = execFileSync(
    "cargo",
    [
      "metadata",
      "--format-version",
      "1",
      // The notices must cover everything the lockfile can ship, including
      // packages behind optional features and other platforms' targets.
      "--all-features",
      "--locked",
      "--manifest-path",
      path.join(root, "Cargo.toml"),
    ],
    { encoding: "utf8", maxBuffer: 512 * 1024 * 1024 },
  );
  return JSON.parse(output);
}

// Enumerate every package pnpm installed under `modulesDirectory`, from its
// virtual store: `node_modules/.pnpm/<instance>/node_modules/<name>` is the
// package itself, a real directory, while its dependencies beside it are
// symlinks into other instances. A package resolved twice with different
// peers has two instances of identical files; they are one package to the
// notices, so instances are deduplicated by name and version.
export function installedPackages(modulesDirectory) {
  const store = path.join(modulesDirectory, ".pnpm");
  if (!existsSync(store)) {
    throw new Error(`no pnpm virtual store at ${store}`);
  }
  const packages = new Map();
  const record = (packageDirectory) => {
    const manifestPath = path.join(packageDirectory, "package.json");
    if (!isFile(manifestPath)) return;
    const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    if (typeof manifest.name !== "string" || typeof manifest.version !== "string") {
      throw new Error(`${manifestPath} names no package or version`);
    }
    const key = `${manifest.name}@${manifest.version}`;
    if (!packages.has(key)) {
      packages.set(key, {
        name: manifest.name,
        version: manifest.version,
        path: packageDirectory,
      });
    }
  };
  const isRealDirectory = (file) => {
    try {
      return lstatSync(file).isDirectory();
    } catch {
      return false;
    }
  };
  for (const instance of readdirSync(store).sort(compareStrings)) {
    // `.pnpm/node_modules` is pnpm's hoisted symlink directory, not a package.
    if (instance === "node_modules") continue;
    const instanceModules = path.join(store, instance, "node_modules");
    if (!isRealDirectory(instanceModules)) continue;
    for (const entry of readdirSync(instanceModules).sort(compareStrings)) {
      const candidate = path.join(instanceModules, entry);
      if (!isRealDirectory(candidate)) continue;
      if (entry.startsWith("@")) {
        for (const scoped of readdirSync(candidate).sort(compareStrings)) {
          const scopedCandidate = path.join(candidate, scoped);
          if (isRealDirectory(scopedCandidate)) record(scopedCandidate);
        }
      } else {
        record(candidate);
      }
    }
  }
  if (packages.size === 0) {
    throw new Error(`pnpm installed no packages under ${modulesDirectory}`);
  }
  return [...packages.values()].sort(comparePackages);
}

export function pnpmInvocation(
  args,
  platform = process.platform,
  commandInterpreter = process.env.ComSpec,
) {
  if (platform === "win32") {
    return {
      executable: commandInterpreter?.trim() || "cmd.exe",
      args: ["/d", "/c", "pnpm", ...args],
    };
  }
  return { executable: "pnpm", args };
}

function runPnpm(args, cwd) {
  // On Windows pnpm is exposed as a .cmd shim, which Node cannot execute
  // directly. Run it through cmd.exe even when this script was launched from
  // Git Bash; Unix hosts keep the direct, shell-free invocation.
  const pnpm = pnpmInvocation(args);
  return execFileSync(pnpm.executable, pnpm.args, {
    cwd,
    encoding: "utf8",
    maxBuffer: 128 * 1024 * 1024,
    stdio: ["ignore", "pipe", "inherit"],
  });
}

// The pnpm-workspace.yaml the scratch project installs with: the UI's own
// settings (overrides above all) plus the platform lists. A project that
// already chooses its architectures would be silently overruled by a second
// key, so that is refused until someone reconciles the two.
export function productionClosureConfig(workspaceConfig) {
  if (/^supportedArchitectures\s*:/m.test(workspaceConfig)) {
    throw new Error(
      `${UI_RELATIVE_PATH}/pnpm-workspace.yaml already sets ` +
        "supportedArchitectures; reconcile it with SUPPORTED_ARCHITECTURES " +
        "in the notices generator",
    );
  }
  const block = [
    "supportedArchitectures:",
    ...Object.entries(SUPPORTED_ARCHITECTURES).map(
      ([key, values]) => `  ${key}: [${values.join(", ")}]`,
    ),
  ].join("\n");
  const base = workspaceConfig.replace(/\s+$/, "");
  return base ? `${base}\n${block}\n` : `${block}\n`;
}

// Install the UI's production closure for every platform into a scratch
// directory and hand the installed packages to `callback`, which must read
// every package file it needs before returning: the directory is removed
// afterwards. The install is `--frozen-lockfile`, so the scratch resolution is
// the checked-in one, and `--ignore-scripts`, because nothing here runs.
function withProductionClosure(uiDirectory, callback) {
  const scratch = mkdtempSync(path.join(tmpdir(), "tidebreak-notices-"));
  try {
    for (const name of UI_CLOSURE_FILES) {
      copyFileSync(path.join(uiDirectory, name), path.join(scratch, name));
    }
    for (const name of UI_CLOSURE_OPTIONAL_FILES) {
      const source = path.join(uiDirectory, name);
      if (isFile(source)) copyFileSync(source, path.join(scratch, name));
    }
    const workspaceFile = path.join(scratch, "pnpm-workspace.yaml");
    writeFileSync(
      workspaceFile,
      productionClosureConfig(
        isFile(workspaceFile) ? readFileSync(workspaceFile, "utf8") : "",
      ),
    );
    runPnpm(
      ["install", "--prod", "--frozen-lockfile", "--ignore-scripts"],
      scratch,
    );
    return callback(installedPackages(path.join(scratch, "node_modules")));
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

export function generateNotices({ root = repositoryRoot } = {}) {
  const uiDirectory = path.join(root, UI_RELATIVE_PATH);
  const uiManifest = JSON.parse(
    readFileSync(path.join(uiDirectory, "package.json"), "utf8"),
  );
  const rustPackages = collectRustPackages(
    mergeCargoMetadata(
      [root, ...EXCLUDED_CARGO_WORKSPACES.map((dir) => path.join(root, dir))].map(
        runCargoMetadata,
      ),
    ),
  );
  const nodePackages = withProductionClosure(uiDirectory, (installed) =>
    collectNodePackages(installed, {
      root,
      // The UI project itself is Tidebreak, covered by LICENSE and NOTICE.
      excludeNames: [uiManifest.name],
    }),
  );
  return renderNotices({ rustPackages, nodePackages });
}

export function firstDifference(expected, actual) {
  const expectedLines = expected.split("\n");
  const actualLines = actual.split("\n");
  const limit = Math.max(expectedLines.length, actualLines.length);
  for (let index = 0; index < limit; index += 1) {
    if (expectedLines[index] !== actualLines[index]) {
      return {
        line: index + 1,
        expected: expectedLines[index] ?? "<end of file>",
        actual: actualLines[index] ?? "<end of file>",
      };
    }
  }
  return null;
}

function main(argv) {
  const check = argv.includes("--check");
  const unknown = argv.filter((argument) => argument !== "--check");
  if (unknown.length > 0) {
    throw new Error("usage: generate-third-party-notices.mjs [--check]");
  }

  const generated = generateNotices();
  const target = path.join(repositoryRoot, NOTICES_RELATIVE_PATH);

  if (!check) {
    writeFileSync(target, generated);
    console.log(`wrote ${NOTICES_RELATIVE_PATH}`);
    return;
  }

  if (!existsSync(target)) {
    throw new Error(
      `${NOTICES_RELATIVE_PATH} is missing; run \`${REGENERATE_COMMAND}\``,
    );
  }
  const current = readFileSync(target, "utf8");
  const comparableCurrent = normalizeNoticesForComparison(current);
  const comparableGenerated = normalizeNoticesForComparison(generated);
  if (comparableCurrent === comparableGenerated) {
    console.log(`${NOTICES_RELATIVE_PATH} is up to date`);
    return;
  }
  const difference = firstDifference(comparableCurrent, comparableGenerated);
  throw new Error(
    `${NOTICES_RELATIVE_PATH} is stale; run \`${REGENERATE_COMMAND}\`\n` +
      `first difference at line ${difference.line}\n` +
      `  checked in: ${difference.expected}\n` +
      `  regenerated: ${difference.actual}`,
  );
}

// Run `main` only when this file is the entry point. Compare real paths so a
// checkout reached through a symlink (hosted runners expose the workspace
// under one) still matches the module's own resolved location.
function isEntryPoint() {
  const entry = process.argv[1];
  if (!entry) return false;
  try {
    return realpathSync(entry) === realpathSync(fileURLToPath(import.meta.url));
  } catch {
    return false;
  }
}

if (isEntryPoint()) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  }
}
