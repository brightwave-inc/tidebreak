import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const script = join(
  dirname(fileURLToPath(import.meta.url)),
  "install-linux-apt-packages.sh",
);

function runFixture({
  arch,
  sources = "",
  extraLists = {},
  mirrors = null,
  extraEnv = {},
  args = ["libwebkit2gtk-4.1-dev"],
} = {}) {
  const root = mkdtempSync(join(tmpdir(), "tidebreak-apt-"));
  try {
    mkdirSync(join(root, "etc/apt/sources.list.d"), { recursive: true });
    mkdirSync(join(root, "etc/apt/apt.conf.d"), { recursive: true });
    if (sources !== null) {
      writeFileSync(join(root, "etc/apt/sources.list"), sources);
    }
    for (const [name, contents] of Object.entries(extraLists)) {
      writeFileSync(join(root, "etc/apt/sources.list.d", name), contents);
    }
    if (mirrors !== null) {
      writeFileSync(join(root, "etc/apt/apt-mirrors.txt"), mirrors);
    }

    const result = spawnSync(script, args, {
      encoding: "utf8",
      env: {
        ...process.env,
        TIDEBREAK_APT_ROOT: root,
        TIDEBREAK_APT_ARCH: arch,
        TIDEBREAK_APT_CODENAME: "jammy",
        TIDEBREAK_APT_DRY_RUN: "1",
        ...extraEnv,
      },
    });
    assert.equal(result.status, 0, result.stderr);

    const read = (relative) => {
      try {
        return readFileSync(join(root, relative), "utf8");
      } catch {
        return null;
      }
    };
    return {
      stdout: result.stdout,
      sources: read("etc/apt/sources.list"),
      extraLists: Object.fromEntries(
        Object.keys(extraLists).map((name) => [
          name,
          read(`etc/apt/sources.list.d/${name}`),
        ]),
      ),
      mirrors: read("etc/apt/apt-mirrors.txt"),
      conf: read("etc/apt/apt.conf.d/99tidebreak-ci"),
    };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

test("rewrites the Azure Ubuntu mirror to archive.ubuntu.com on amd64", () => {
  const result = runFixture({
    arch: "amd64",
    sources: [
      "deb http://azure.archive.ubuntu.com/ubuntu jammy main restricted",
      "deb http://azure.archive.ubuntu.com/ubuntu jammy-updates main restricted",
      "deb http://azure.archive.ubuntu.com/ubuntu jammy-backports main restricted",
      "deb http://azure.archive.ubuntu.com/ubuntu jammy-security main restricted",
      "",
    ].join("\n"),
    extraLists: {
      "ubuntu.sources":
        "URIs: http://azure.archive.ubuntu.com/ubuntu\nSuites: jammy jammy-updates jammy-backports\n",
    },
    mirrors: "http://azure.archive.ubuntu.com/ubuntu\tpriority:1\n",
  });

  assert.match(result.sources, /deb http:\/\/archive\.ubuntu\.com\/ubuntu jammy main/);
  assert.doesNotMatch(result.sources, /azure\.archive\.ubuntu\.com/);
  assert.match(
    result.extraLists["ubuntu.sources"],
    /URIs: http:\/\/archive\.ubuntu\.com\/ubuntu/,
  );
  assert.equal(
    result.mirrors,
    "http://archive.ubuntu.com/ubuntu\tpriority:1\n",
  );
  assert.match(result.conf, /Acquire::Retries "3";/);
  assert.match(result.conf, /Acquire::http::Timeout "20";/);
  assert.match(
    result.stdout,
    /would run: apt-get .*Acquire::http::Timeout=20.* update/,
  );
  assert.match(
    result.stdout,
    /install -y --no-install-recommends libwebkit2gtk-4\.1-dev/,
  );
});

test("rewrites ARM runners to ports.ubuntu.com", () => {
  const result = runFixture({
    arch: "arm64",
    sources: "deb http://azure.archive.ubuntu.com/ubuntu jammy main\n",
    extraLists: {
      "ubuntu.list": "deb http://azure.archive.ubuntu.com/ubuntu-ports jammy main\n",
    },
  });

  assert.match(result.sources, /deb http:\/\/ports\.ubuntu\.com\/ubuntu-ports jammy main/);
  assert.match(
    result.extraLists["ubuntu.list"],
    /deb http:\/\/ports\.ubuntu\.com\/ubuntu-ports jammy main/,
  );
  assert.equal(
    result.mirrors,
    "http://ports.ubuntu.com/ubuntu-ports\tpriority:1\n",
  );
});

test("writes a public archive sources.list when none exists", () => {
  const result = runFixture({
    arch: "amd64",
    sources: null,
  });

  assert.equal(
    result.sources,
    [
      "deb http://archive.ubuntu.com/ubuntu jammy main restricted universe multiverse",
      "deb http://archive.ubuntu.com/ubuntu jammy-updates main restricted universe multiverse",
      "deb http://archive.ubuntu.com/ubuntu jammy-backports main restricted universe multiverse",
      "deb http://security.ubuntu.com/ubuntu jammy-security main restricted universe multiverse",
      "",
    ].join("\n"),
  );
});

test("rejects an empty package list", () => {
  const result = spawnSync(script, [], { encoding: "utf8" });
  assert.equal(result.status, 2);
  assert.match(result.stderr, /usage: scripts\/install-linux-apt-packages\.sh/);
});

test("the helper is executable", () => {
  chmodSync(script, 0o755);
  const result = spawnSync("test", ["-x", script]);
  assert.equal(result.status, 0);
});
