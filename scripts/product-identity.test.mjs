import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import test from "node:test";

const retiredName = ["open", "wave"].join("");

function git(...args) {
  return execFileSync("git", args, { encoding: "utf8" });
}

test("tracked files use Tidebreak as the sole product identity", () => {
  const retiredFilenames = git("ls-files")
    .split("\n")
    .filter((path) => path.toLowerCase().includes(retiredName));
  assert.deepEqual(retiredFilenames, [], "a tracked filename uses the retired product name");

  let matches = "";
  try {
    matches = git(
      "grep",
      "-I",
      "-n",
      "-i",
      "-e",
      retiredName,
      "--",
      ":(exclude)docs/decisions/**",
    );
  } catch (error) {
    assert.equal(error.status, 1, error.stderr || error.message);
  }
  assert.equal(matches, "", `the retired product name remains outside decision records:\n${matches}`);
});
