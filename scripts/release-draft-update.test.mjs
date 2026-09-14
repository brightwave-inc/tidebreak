import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
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
const script = step
  .split("        run: |\n")[1]
  .split("\n")
  .map((line) => line.replace(/^          /, ""))
  .join("\n");
const releaseSha = "a".repeat(40);

// Run the workflow's shell against a local GitHub stub. Nothing reaches GitHub.
for (const response of [
  "valid_branch", "valid_sha", "tag", "target", "published", "string_flags",
]) {
  const name = response.startsWith("valid_")
    ? `draft update keeps existing ${response.slice(6)} target metadata`
    : `draft update rejects ${response} drift`;
  test(name, () => {
    const directory = mkdtempSync(join(tmpdir(), "tidebreak-release-draft-"));
    try {
      const payload = join(directory, "payload.json");
      const target = response === "valid_sha" ? releaseSha : "main";
      writeFileSync(
        join(directory, "release-snapshot.json"),
        JSON.stringify({ target_commitish: target }),
      );
      const invocation = `
gh() {
  cat > "$PAYLOAD"
  case "$RESPONSE" in
    tag) jq '. + {tag_name: "untagged-rebound"}' "$PAYLOAD" ;;
    target) jq '. + {target_commitish: "different-target"}' "$PAYLOAD" ;;
    published) jq '. + {draft: false}' "$PAYLOAD" ;;
    string_flags) jq '. + {draft: "true", prerelease: "true"}' "$PAYLOAD" ;;
    # GitHub keeps an existing tag's target metadata instead of retargeting it.
    *) jq --arg target "$METADATA_TARGET" '. + {target_commitish: $target}' "$PAYLOAD" ;;
  esac
}
${script}`;
      const result = spawnSync("bash", [
        "--noprofile", "--norc", "-e", "-o", "pipefail", "-c", invocation,
      ], {
        encoding: "utf8",
        env: {
          PATH: process.env.PATH,
          RELEASE_ID: "123",
          RELEASE_TAG: "v0.103.6",
          RUNNER_TEMP: directory,
          METADATA_TARGET: target,
          GITHUB_REPOSITORY: "example/tidebreak",
          PAYLOAD: payload,
          RESPONSE: response,
        },
      });
      assert.deepEqual(JSON.parse(readFileSync(payload, "utf8")), {
        draft: true,
        prerelease: true,
        tag_name: "v0.103.6",
        target_commitish: target,
      });
      if (response.startsWith("valid_")) {
        assert.equal(result.status, 0, result.stderr);
      } else {
        assert.notEqual(result.status, 0, `the workflow accepted ${response} drift`);
      }
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });
}
