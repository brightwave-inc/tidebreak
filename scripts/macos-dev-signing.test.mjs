import assert from "node:assert/strict";
import {
  chmod,
  mkdtemp,
  mkdir,
  readFile,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import test from "node:test";

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const runner = join(scriptsDir, "macos-dev-sign-runner.sh");

/// Stand up a fake `security`/`codesign`/target binary on PATH and run the
/// runner over it, returning every command it issued. `identities` is what the
/// fake `security find-identity` prints.
async function runWithIdentities(root, identities) {
  const binDir = join(root, "bin");
  const log = join(root, "commands.log");
  const probe = join(root, "probe");
  await mkdir(binDir);

  const security = join(binDir, "security");
  await writeFile(
    security,
    `#!/usr/bin/env bash
set -euo pipefail
printf 'security %s\\n' "$*" >>"$TIDEBREAK_TEST_LOG"
case "$1" in
  find-identity)
    printf '%s' "$TIDEBREAK_TEST_IDENTITIES"
    ;;
  create-keychain)
    keychain="\${!#}"
    mkdir -p "$(dirname "$keychain")"
    : >"$keychain"
    ;;
  unlock-keychain|find-certificate|find-key)
    keychain="\${!#}"
    [[ -f "$keychain" ]]
    ;;
  list-keychains)
    if [[ " $* " != *" -s "* ]]; then
      printf '    "%s"\\n' "$TIDEBREAK_TEST_LOGIN_KEYCHAIN"
    fi
    ;;
esac
`,
  );

  const codesign = join(binDir, "codesign");
  await writeFile(
    codesign,
    `#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" == "--display" ]]; then
  exit 1
fi
printf 'codesign %s\\n' "$*" >>"$TIDEBREAK_TEST_LOG"
`,
  );

  await writeFile(
    probe,
    `#!/usr/bin/env bash
printf 'exec %s\\n' "$*" >>"$TIDEBREAK_TEST_LOG"
`,
  );
  await Promise.all([
    chmod(security, 0o755),
    chmod(codesign, 0o755),
    chmod(probe, 0o755),
  ]);

  const env = {
    ...process.env,
    TIDEBREAK_DEV_SIGNING_DIR: join(root, "signing"),
    TIDEBREAK_TEST_LOG: log,
    TIDEBREAK_TEST_IDENTITIES: identities,
    TIDEBREAK_TEST_LOGIN_KEYCHAIN: join(root, "login.keychain-db"),
    PATH: `${binDir}:${process.env.PATH}`,
  };

  for (const argument of ["first", "second"]) {
    const result = spawnSync(runner, [probe, argument], {
      encoding: "utf8",
      env,
    });
    assert.equal(result.status, 0, result.stderr);
  }

  return readFile(log, "utf8");
}

// A team identifier is the whole point: without one, macOS pins the keychain
// approval to the binary's cdhash and the next rebuild prompts again. So a
// team-identified certificate has to beat the local-only fallback, even though
// it means development signs with a distribution key.
test("prefers a team-identified identity over bootstrapping a local one", async () => {
  const root = await mkdtemp(join(tmpdir(), "tidebreak-dev-signing-"));
  try {
    const commands = await runWithIdentities(
      root,
      '  1) ABCDEF "Developer ID Application: Example (TEAMID)"\n     1 valid identities found\n',
    );

    assert.equal(commands.match(/security create-keychain/g), null);
    assert.equal(
      commands.match(
        /codesign --force --sign Developer ID Application: Example \(TEAMID\)/g,
      )?.length,
      2,
    );
    assert.match(commands, /codesign .*--identifier tidebreak-dev/);
    assert.match(commands, /exec first/);
    assert.match(commands, /exec second/);
  } finally {
    await rm(root, { force: true, recursive: true });
  }
});

test("prefers Apple Development over Developer ID", async () => {
  const root = await mkdtemp(join(tmpdir(), "tidebreak-dev-signing-"));
  try {
    const commands = await runWithIdentities(
      root,
      '  1) ABCDEF "Developer ID Application: Example (TEAMID)"\n' +
        '  2) 123456 "Apple Development: dev@example.com (OTHERID)"\n' +
        "     2 valid identities found\n",
    );

    assert.match(
      commands,
      /codesign --force --sign Apple Development: dev@example\.com \(OTHERID\)/,
    );
    assert.equal(commands.match(/Developer ID Application/g), null);
  } finally {
    await rm(root, { force: true, recursive: true });
  }
});

test("bootstraps local signing when no identity exists", async () => {
  const root = await mkdtemp(join(tmpdir(), "tidebreak-dev-signing-"));
  try {
    const commands = await runWithIdentities(root, "     0 valid identities found\n");

    assert.equal(commands.match(/security create-keychain/g)?.length, 1);
    assert.equal(commands.match(/security import/g)?.length, 1);
    assert.equal(
      commands.match(/codesign --force --sign tidebreak-dev/g)?.length,
      2,
    );
    assert.match(commands, /exec first/);
    assert.match(commands, /exec second/);
  } finally {
    await rm(root, { force: true, recursive: true });
  }
});
