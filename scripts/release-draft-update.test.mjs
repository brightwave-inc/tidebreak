import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const workflow = readFileSync(
  new URL("../.github/workflows/release.yml", import.meta.url),
  "utf8",
);
const step = workflow
  .split("      - name: Mark the in-flight draft as a prerelease\n")[1]
  ?.split("      - name: Require a commit from main\n")[0];
assert.ok(step, "the draft update step must exist");
const script = step.split("        run: |\n")[1]
  .split("\n")
  .map((line) => line.replace(/^          /, ""))
  .join("\n");
const releaseSha = "a".repeat(40);

// Run the workflow's shell against a local GitHub stub. Nothing reaches GitHub.
for (const response of ["valid", "tag", "source", "published", "string_flags"]) {
  const name = response === "valid"
    ? "draft update keeps the frozen tag and source"
    : `draft update rejects ${response} drift`;
  test(name, () => {
    const directory = mkdtempSync(join(tmpdir(), "tidebreak-release-draft-"));
    try {
      const payload = join(directory, "payload.json");
      const result = spawnSync("bash", ["--noprofile", "--norc", "-e", "-o", "pipefail", "-c", `
gh() {
  cat > "$PAYLOAD"
  case "$RESPONSE" in
    tag) jq '. + {tag_name: "untagged-rebound"}' "$PAYLOAD" ;;
    source) jq '. + {target_commitish: "main"}' "$PAYLOAD" ;;
    published) jq '. + {draft: false}' "$PAYLOAD" ;;
    string_flags) jq '. + {draft: "true", prerelease: "true"}' "$PAYLOAD" ;;
    *) cat "$PAYLOAD" ;;
  esac
}
${script}`], {
        encoding: "utf8",
        env: {
          PATH: process.env.PATH,
          RELEASE_ID: "123",
          RELEASE_TAG: "v0.103.6",
          RELEASE_SHA: releaseSha,
          GITHUB_REPOSITORY: "example/tidebreak",
          PAYLOAD: payload,
          RESPONSE: response,
        },
      });
      assert.deepEqual(JSON.parse(readFileSync(payload, "utf8")), {
        draft: true,
        prerelease: true,
        tag_name: "v0.103.6",
        target_commitish: releaseSha,
      });
      if (response === "valid") {
        assert.equal(result.status, 0, result.stderr);
      } else {
        assert.notEqual(result.status, 0, `the workflow accepted ${response} drift`);
      }
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });
}
