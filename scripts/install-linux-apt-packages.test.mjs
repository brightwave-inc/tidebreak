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

  assert.match(
    result.sources,
    /deb https:\/\/archive\.ubuntu\.com\/ubuntu jammy main/,
  );
  assert.doesNotMatch(result.sources, /azure\.archive\.ubuntu\.com/);
  assert.match(
    result.extraLists["ubuntu.sources"],
    /URIs: https:\/\/archive\.ubuntu\.com\/ubuntu/,
  );
  assert.equal(
    result.mirrors,
    "https://archive.ubuntu.com/ubuntu\tpriority:1\n",
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
      "ubuntu.list":
        "deb http://azure.archive.ubuntu.com/ubuntu-ports jammy main\n",
    },
  });

  assert.match(
    result.sources,
    /deb https:\/\/ports\.ubuntu\.com\/ubuntu-ports jammy main/,
  );
  assert.match(
    result.extraLists["ubuntu.list"],
    /deb https:\/\/ports\.ubuntu\.com\/ubuntu-ports jammy main/,
  );
  assert.equal(
    result.mirrors,
    "https://ports.ubuntu.com/ubuntu-ports\tpriority:1\n",
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
      "deb https://archive.ubuntu.com/ubuntu jammy main restricted universe multiverse",
      "deb https://archive.ubuntu.com/ubuntu jammy-updates main restricted universe multiverse",
      "deb https://archive.ubuntu.com/ubuntu jammy-backports main restricted universe multiverse",
      "deb https://security.ubuntu.com/ubuntu jammy-security main restricted universe multiverse",
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

test("normalizes existing official Ubuntu HTTP sources to HTTPS", () => {
  const result = runFixture({
    arch: "amd64",
    sources: [
      "deb http://archive.ubuntu.com/ubuntu jammy main",
      "deb http://security.ubuntu.com/ubuntu jammy-security main",
      "deb http://custom.example/ubuntu jammy main",
      "",
    ].join("\n"),
    extraLists: {
      "ubuntu.sources":
        "URIs: http://archive.ubuntu.com/ubuntu\nSuites: jammy jammy-updates\n",
      "security.list":
        "deb http://security.ubuntu.com/ubuntu jammy-security main\n",
    },
    mirrors: "http://archive.ubuntu.com/ubuntu\tpriority:1\n",
  });

  assert.equal(
    result.sources,
    [
      "deb https://archive.ubuntu.com/ubuntu jammy main",
      "deb https://security.ubuntu.com/ubuntu jammy-security main",
      "deb http://custom.example/ubuntu jammy main",
      "",
    ].join("\n"),
  );
  assert.equal(
    result.extraLists["ubuntu.sources"],
    "URIs: https://archive.ubuntu.com/ubuntu\nSuites: jammy jammy-updates\n",
  );
  assert.equal(
    result.extraLists["security.list"],
    "deb https://security.ubuntu.com/ubuntu jammy-security main\n",
  );
  assert.equal(
    result.mirrors,
    "https://archive.ubuntu.com/ubuntu\tpriority:1\n",
  );
});

test("normalizes existing Ubuntu ports HTTP sources to HTTPS", () => {
  const result = runFixture({
    arch: "arm64",
    sources: "deb http://ports.ubuntu.com/ubuntu-ports jammy main\n",
    extraLists: {
      "ubuntu.sources":
        "URIs: http://ports.ubuntu.com/ubuntu-ports\nSuites: jammy\n",
    },
  });

  assert.equal(
    result.sources,
    "deb https://ports.ubuntu.com/ubuntu-ports jammy main\n",
  );
  assert.equal(
    result.extraLists["ubuntu.sources"],
    "URIs: https://ports.ubuntu.com/ubuntu-ports\nSuites: jammy\n",
  );
  assert.equal(
    result.mirrors,
    "https://ports.ubuntu.com/ubuntu-ports\tpriority:1\n",
  );
});

test("preserves explicit endpoint overrides without rewriting their output", () => {
  const result = runFixture({
    arch: "amd64",
    sources: [
      "deb http://archive.ubuntu.com/ubuntu jammy main",
      "deb https://security.ubuntu.com/ubuntu jammy-security main",
      "",
    ].join("\n"),
    extraEnv: {
      TIDEBREAK_APT_ARCHIVE_URL: "http://security.ubuntu.com/ubuntu",
      TIDEBREAK_APT_SECURITY_URL: "http://archive.ubuntu.com/ubuntu",
    },
  });

  assert.equal(
    result.sources,
    [
      "deb http://security.ubuntu.com/ubuntu jammy main",
      "deb http://archive.ubuntu.com/ubuntu jammy-security main",
      "",
    ].join("\n"),
  );
  assert.equal(
    result.mirrors,
    "http://security.ubuntu.com/ubuntu\tpriority:1\n",
  );
});

test("keeps an existing Ubuntu deb822 source without adding sources.list", () => {
  const result = runFixture({
    arch: "amd64",
    sources: null,
    extraLists: {
      "ubuntu.sources": [
        "Types: deb",
        "URIs: mirror+file:/etc/apt/apt-mirrors.txt",
        "Suites: jammy jammy-updates jammy-security",
        "Components: main restricted universe multiverse",
        "Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg",
        "",
      ].join("\n"),
    },
  });

  assert.equal(result.sources, null);
  assert.match(
    result.extraLists["ubuntu.sources"],
    /Signed-By: \/usr\/share\/keyrings\/ubuntu-archive-keyring\.gpg/,
  );
  assert.match(
    result.extraLists["ubuntu.sources"],
    /Suites: jammy jammy-updates jammy-security/,
  );
});

function runCommandFixture({ plan = {}, extraEnv = {} } = {}) {
  const root = mkdtempSync(join(tmpdir(), "tidebreak-apt-commands-"));
  try {
    const bin = join(root, "bin");
    const log = join(root, "commands.json");
    mkdirSync(bin);
    mkdirSync(join(root, "etc/apt/sources.list.d"), { recursive: true });
    writeFileSync(
      join(root, "etc/apt/sources.list"),
      "deb http://archive.ubuntu.com/ubuntu jammy main\n",
    );
    writeFileSync(log, "[]");
    const stub = [
      "#!" + process.execPath,
      'const fs = require("node:fs");',
      'const mode = require("node:path").basename(process.argv[1]);',
      'if (mode === "id") { console.log("0"); process.exit(0); }',
      "const args = process.argv.slice(2);",
      "const log = process.env.TIDEBREAK_APT_COMMAND_LOG;",
      'const records = JSON.parse(fs.readFileSync(log, "utf8"));',
      'const stage = args.includes("update") ? "update" : args.includes("--download-only") ? "download" : "install";',
      'const attempt = records.filter(row => row.command === "apt-get" && row.stage === stage).length;',
      'const mirrors = fs.readFileSync(process.env.TIDEBREAK_APT_ROOT + "/etc/apt/apt-mirrors.txt", "utf8");',
      "records.push({ command: mode, args, stage, mirrors });",
      "fs.writeFileSync(log, JSON.stringify(records));",
      'if (mode === "timeout") {',
      '  const child = require("node:child_process").spawnSync(args[2], args.slice(3), { stdio: "inherit", env: process.env });',
      "  process.exit(child.status ?? 1);",
      "}",
      "const plan = JSON.parse(process.env.TIDEBREAK_APT_COMMAND_PLAN);",
      "process.exit(plan[stage]?.[attempt] ?? 0);",
      "",
    ].join("\n");
    for (const name of ["id", "timeout", "apt-get"]) {
      writeFileSync(join(bin, name), stub, { mode: 0o755 });
    }
    const result = spawnSync(script, ["libwebkit2gtk-4.1-dev", "mold"], {
      encoding: "utf8",
      env: {
        ...process.env,
        PATH: bin + ":" + process.env.PATH,
        TIDEBREAK_APT_ROOT: root,
        TIDEBREAK_APT_ARCH: "amd64",
        TIDEBREAK_APT_CODENAME: "jammy",
        TIDEBREAK_APT_DRY_RUN: "",
        TIDEBREAK_APT_COMMAND_LOG: log,
        TIDEBREAK_APT_COMMAND_PLAN: JSON.stringify(plan),
        ...extraEnv,
      },
    });
    const commands = JSON.parse(readFileSync(log, "utf8"));
    return {
      ...result,
      commands,
      apt: commands.filter((row) => row.command === "apt-get"),
    };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

test("uses the HTTPS fallback after an index refresh times out", () => {
  const result = runCommandFixture({ plan: { update: [124, 0] } });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(
    result.apt.map((row) => row.stage),
    ["update", "update", "download", "install"],
  );
  assert.equal(
    result.apt[0].mirrors,
    "https://archive.ubuntu.com/ubuntu\tpriority:1\n",
  );
  assert.equal(
    result.apt[1].mirrors,
    "https://mirrors.edge.kernel.org/ubuntu\tpriority:1\n",
  );
  for (const row of result.apt.filter((row) => row.stage === "update")) {
    assert.ok(row.args.includes("APT::Update::Error-Mode=any"));
  }
  assert.ok(result.apt.at(-1).args.includes("--no-download"));
});

test("bounds both mirror download phases and preserves the package arguments", () => {
  const result = runCommandFixture({ plan: { download: [124, 0] } });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(
    result.apt.map((row) => row.stage),
    ["update", "download", "update", "download", "install"],
  );
  assert.deepEqual(
    result.commands
      .filter((row) => row.command === "timeout")
      .map((row) => row.args.slice(0, 2)),
    [
      ["--kill-after=5s", "60s"],
      ["--kill-after=5s", "100s"],
      ["--kill-after=5s", "60s"],
      ["--kill-after=5s", "100s"],
    ],
  );
  for (const row of result.apt.filter((row) => row.stage !== "update")) {
    assert.deepEqual(row.args.slice(-2), ["libwebkit2gtk-4.1-dev", "mold"]);
  }
  assert.ok(result.apt.at(-1).args.includes("--no-download"));
  assert.equal(result.commands.at(-1).command, "apt-get");
});

test("fails after both mirror refreshes fail without installing packages", () => {
  const result = runCommandFixture({ plan: { update: [124, 100] } });
  assert.equal(result.status, 1);
  assert.deepEqual(
    result.apt.map((row) => row.stage),
    ["update", "update"],
  );
  assert.match(result.stderr, /failed on both HTTPS mirrors/);
});

test("does not retry a local installation failure on another mirror", () => {
  const result = runCommandFixture({ plan: { install: [100] } });
  assert.equal(result.status, 100);
  assert.deepEqual(
    result.apt.map((row) => row.stage),
    ["update", "download", "install"],
  );
  assert.ok(result.apt.every((row) => !row.mirrors.includes("kernel.org")));
  assert.equal(result.commands.at(-1).command, "apt-get");
});

test("keeps explicit endpoint overrides outside automatic mirror fallback", () => {
  const result = runCommandFixture({
    plan: { install: [100] },
    extraEnv: { TIDEBREAK_APT_ARCHIVE_URL: "https://custom.example/ubuntu" },
  });
  assert.equal(result.status, 100);
  assert.deepEqual(
    result.apt.map((row) => row.stage),
    ["update", "install"],
  );
  assert.ok(result.commands.every((row) => row.command === "apt-get"));
  assert.ok(
    result.apt.every(
      (row) => row.mirrors === "https://custom.example/ubuntu\tpriority:1\n",
    ),
  );
});
