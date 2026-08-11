#!/usr/bin/env node
// Generate legal/THIRD-PARTY-NOTICES.md from the resolved dependency graphs the
// product actually ships: the Cargo workspace's non-member packages and the
// desktop UI's production npm closure.
//
// The generated file is checked in and bundled into the desktop app, so the
// only property that matters as much as completeness is determinism: the same
// lockfiles must always produce byte-identical output. Two rules keep that
// true. Nothing derived from the host — absolute paths, timestamps, package
// counts of the local checkout — reaches the output; and anything that could
// silently reduce coverage (an unpacked source tree that is missing, a package
// manager that reports no graph) is a hard error rather than a smaller file.
//
// Dependency-light on purpose: `cargo metadata` and `pnpm licenses list` are
// already required to build the product, and both are invoked as pure graph
// oracles. License facts are read from each package's own vendored files and
// manifest, never from a tool's classification, so upgrading either tool
// cannot rewrite the notices.

import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import {
  existsSync,
  readFileSync,
  readdirSync,
  statSync,
  writeFileSync,
} from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const repositoryRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
);

export const NOTICES_RELATIVE_PATH = "legal/THIRD-PARTY-NOTICES.md";
export const UI_RELATIVE_PATH = "crates/openwave-desktop/ui";
export const REGENERATE_COMMAND = "node scripts/generate-third-party-notices.mjs";

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

// `pnpm licenses list --json --prod` is used only to enumerate the production
// closure and locate each package on disk. Its own license classification is
// deliberately ignored: it reads the manifest we read ourselves, and its
// heuristics are free to change between pnpm releases.
export function collectNodePackages(
  pnpmLicenses,
  { excludeNames = [], root = repositoryRoot } = {},
) {
  const excluded = new Set(excludeNames);
  const packages = new Map();
  for (const entries of Object.values(pnpmLicenses)) {
    for (const entry of entries) {
      if (excluded.has(entry.name)) continue;
      const versions = entry.versions ?? [];
      const paths = entry.paths ?? [];
      if (versions.length !== paths.length) {
        throw new Error(
          `pnpm reported ${versions.length} versions and ${paths.length} paths for ${entry.name}`,
        );
      }
      versions.forEach((version, index) => {
        const packageDirectory = paths[index];
        const manifestPath = path.join(packageDirectory, "package.json");
        if (!isFile(manifestPath)) {
          throw new Error(
            `no installed package for ${entry.name} ${version} at ${packageDirectory}; ` +
              "run `pnpm install --frozen-lockfile` and regenerate",
          );
        }
        const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
        const declaredLicense = manifestLicense(manifest);
        const repository = manifestRepository(manifest);
        const distributedTexts = collectLicenseTexts(packageDirectory, null);
        const curated = curatedNodeLicense(
          { name: entry.name, declaredLicense, repository },
          { root },
        );
        packages.set(`${entry.name}@${version}`, {
          name: entry.name,
          version,
          license: curated?.license ?? declaredLicense ?? UNDECLARED_LICENSE,
          licenseNote: curated?.note ?? null,
          repository,
          licenseTexts: [...distributedTexts, ...(curated?.licenseTexts ?? [])],
        });
      });
    }
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
    "OpenWave is distributed with the third-party software listed below. This",
    "file is generated; do not edit it by hand. Regenerate it with:",
    "",
    "```",
    REGENERATE_COMMAND,
    "```",
    "",
    "It covers every package in the resolved Cargo workspace graph that is not",
    "an OpenWave crate, and every package in the desktop UI's production",
    "dependency graph. Development-only dependencies are excluded because they",
    "are not distributed.",
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

function runPnpmLicenses(uiDirectory) {
  const output = execFileSync(
    "pnpm",
    ["licenses", "list", "--json", "--prod"],
    { cwd: uiDirectory, encoding: "utf8", maxBuffer: 128 * 1024 * 1024 },
  );
  const parsed = JSON.parse(output);
  if (!parsed || typeof parsed !== "object" || Object.keys(parsed).length === 0) {
    throw new Error(
      `pnpm reported no production dependencies in ${uiDirectory}; ` +
        "run `pnpm install --frozen-lockfile` and regenerate",
    );
  }
  return parsed;
}

export function generateNotices({ root = repositoryRoot } = {}) {
  const uiDirectory = path.join(root, UI_RELATIVE_PATH);
  const uiManifest = JSON.parse(
    readFileSync(path.join(uiDirectory, "package.json"), "utf8"),
  );
  return renderNotices({
    rustPackages: collectRustPackages(runCargoMetadata(root)),
    nodePackages: collectNodePackages(runPnpmLicenses(uiDirectory), {
      root,
      // The UI project itself is OpenWave, covered by LICENSE and NOTICE.
      excludeNames: [uiManifest.name],
    }),
  });
}

function firstDifference(expected, actual) {
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
  if (current === generated) {
    console.log(`${NOTICES_RELATIVE_PATH} is up to date`);
    return;
  }
  const difference = firstDifference(current, generated);
  throw new Error(
    `${NOTICES_RELATIVE_PATH} is stale; run \`${REGENERATE_COMMAND}\`\n` +
      `first difference at line ${difference.line}\n` +
      `  checked in: ${difference.expected}\n` +
      `  regenerated: ${difference.actual}`,
  );
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  }
}
