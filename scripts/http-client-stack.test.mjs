import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const lockfile = readFileSync(new URL("../Cargo.lock", import.meta.url), "utf8");
const packages = lockfile
  .split(/^\[\[package\]\]$/m)
  .slice(1)
  .map((entry) => ({
    name: entry.match(/^name = "([^"]+)"$/m)?.[1],
    version: entry.match(/^version = "([^"]+)"$/m)?.[1],
    dependencies: entry.match(/^dependencies = \[([\s\S]*?)^\]/m)?.[1] ?? "",
  }));

// The updater and workspace clients must share one HTTP stack. A second
// reqwest version adds compilation work to every desktop build.
for (const name of ["reqwest", "hyper", "hyper-rustls"]) {
  test(`the lockfile resolves one ${name} version`, () => {
    const versions = packages.filter((entry) => entry.name === name);
    assert.equal(
      versions.length,
      1,
      `expected one ${name} version, found: ${versions.map((entry) => entry.version).join(", ")}`,
    );
  });
}

// reqwest 0.13's rustls feature supplies the platform certificate verifier.
// Keep that verifier in the resolved client graph when changing HTTP features.
test("reqwest retains platform certificate verification", () => {
  const reqwest = packages.find((entry) => entry.name === "reqwest");
  assert.ok(reqwest);
  assert.match(reqwest.dependencies, /^ "rustls-platform-verifier",$/m);
});
