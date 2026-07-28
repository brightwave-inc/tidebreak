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

test("bootstraps and reuses local signing when only Developer ID exists", async () => {
  const root = await mkdtemp(join(tmpdir(), "openwave-dev-signing-"));
  const binDir = join(root, "bin");
  const log = join(root, "commands.log");
  const probe = join(root, "probe");
  await mkdir(binDir);

  try {
    const security = join(binDir, "security");
    await writeFile(
      security,
      `#!/usr/bin/env bash
set -euo pipefail
printf 'security %s\\n' "$*" >>"$OPENWAVE_TEST_LOG"
case "$1" in
  find-identity)
    printf '  1) ABCDEF "Developer ID Application: Example (TEAMID)"\\n'
    printf '     1 valid identities found\\n'
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
      printf '    "%s"\\n' "$OPENWAVE_TEST_LOGIN_KEYCHAIN"
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
printf 'codesign %s\\n' "$*" >>"$OPENWAVE_TEST_LOG"
`,
    );

    await writeFile(
      probe,
      `#!/usr/bin/env bash
printf 'exec %s\\n' "$*" >>"$OPENWAVE_TEST_LOG"
`,
    );
    await Promise.all([
      chmod(security, 0o755),
      chmod(codesign, 0o755),
      chmod(probe, 0o755),
    ]);

    const env = {
      ...process.env,
      OPENWAVE_DEV_SIGNING_DIR: join(root, "signing"),
      OPENWAVE_TEST_LOG: log,
      OPENWAVE_TEST_LOGIN_KEYCHAIN: join(root, "login.keychain-db"),
      PATH: `${binDir}:${process.env.PATH}`,
    };

    for (const argument of ["first", "second"]) {
      const result = spawnSync(runner, [probe, argument], {
        encoding: "utf8",
        env,
      });
      assert.equal(result.status, 0, result.stderr);
    }

    const commands = await readFile(log, "utf8");
    assert.equal(commands.match(/security create-keychain/g)?.length, 1);
    assert.equal(commands.match(/security import/g)?.length, 1);
    assert.equal(commands.match(/security find-identity/g)?.length, 1);
    assert.equal(commands.match(/codesign --force --sign openwave-dev/g)?.length, 2);
    assert.match(commands, /codesign .*--identifier openwave-dev/);
    assert.match(commands, /exec first/);
    assert.match(commands, /exec second/);
  } finally {
    await rm(root, { force: true, recursive: true });
  }
});
