import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

// deploy/self-host/setup.sh prepares a deployment directory for
// docker-compose.yml. These tests run it with flags only (stdin is not a
// terminal, so it never prompts) and pin what an operator relies on: the
// files it writes, their modes, the release it picks, and that it never
// replaces a file that exists.

const setup = fileURLToPath(new URL("../deploy/self-host/setup.sh", import.meta.url));
const compose = readFileSync(
  new URL("../deploy/self-host/docker-compose.yml", import.meta.url),
  "utf8",
);

// The first release that keeps blobs on local disk, which the compose file
// does by default.
const minimum = "0.117.0";

// On Linux the server's uid reads tokens and secret.key through the owner's
// group, which docker-compose.yml adds to the container. Docker Desktop and
// OrbStack on macOS share files with any container uid, so there they stay
// owner-only. Run as root, the script gives both files to the server's uid.
const linux = process.platform !== "darwin";
const root = typeof process.getuid === "function" && process.getuid() === 0;
const sharedMode = linux && !root ? 0o640 : 0o600;

function scratch(t) {
  const dir = mkdtempSync(path.join(tmpdir(), "tidebreak-setup-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

/**
 * A `curl` that answers GitHub's latest-release lookup with `tag`, or fails
 * like a machine without network access when `tag` is null.
 */
function fakeCurl(t, tag) {
  const bin = scratch(t);
  const body =
    tag === null
      ? "#!/bin/sh\nexit 6\n"
      : `#!/bin/sh\nprintf '{"url":"https://api.github.invalid/releases/1","tag_name":"%s","draft":false}\\n' '${tag}'\n`;
  writeFileSync(path.join(bin, "curl"), body);
  chmodSync(path.join(bin, "curl"), 0o755);
  return { PATH: `${bin}${path.delimiter}${process.env.PATH}` };
}

function run(dir, args, { env = {}, cwd } = {}) {
  return spawnSync("sh", [setup, "--dir", dir, ...args], {
    cwd,
    encoding: "utf8",
    env: { ...process.env, ...env },
    stdio: ["ignore", "pipe", "pipe"],
  });
}

function mode(file) {
  return statSync(file).mode & 0o777;
}

/** `.env` as a map, ignoring comments. */
function readEnv(dir) {
  const entries = readFileSync(path.join(dir, ".env"), "utf8")
    .split("\n")
    .filter((line) => line && !line.startsWith("#"))
    .map((line) => {
      const at = line.indexOf("=");
      return [line.slice(0, at), line.slice(at + 1)];
    });
  return new Map(entries);
}

test("setup writes .env, tokens, and secret.key, private, and shows the token once", (t) => {
  const dir = scratch(t);
  const result = run(dir, [
    "--admin",
    "alice",
    "--version",
    "v0.117.0",
    "--domain",
    "tidebreak.example.com",
  ]);
  assert.equal(result.status, 0, result.stderr);

  assert.equal(mode(path.join(dir, ".env")), 0o600);
  assert.equal(mode(path.join(dir, "tokens")), sharedMode);
  assert.equal(mode(path.join(dir, "secret.key")), sharedMode);

  const env = readEnv(dir);
  if (linux && !root) {
    // The group the files carry is the one .env tells compose to add.
    assert.equal(statSync(path.join(dir, "tokens")).gid, process.getgid());
    assert.equal(statSync(path.join(dir, "secret.key")).gid, process.getgid());
    assert.equal(env.get("TIDEBREAK_HOST_GID"), String(process.getgid()));
  }
  assert.equal(env.get("TIDEBREAK_VERSION"), "0.117.0");
  assert.equal(env.has("COMPOSE_FILE"), false);
  assert.match(env.get("POSTGRES_PASSWORD"), /^[0-9a-f]{64}$/);
  assert.equal(env.get("TIDEBREAK_DOMAIN"), "tidebreak.example.com");
  assert.equal(env.get("TIDEBREAK_PUBLIC_URL"), "https://tidebreak.example.com");
  assert.equal(env.get("COMPOSE_PROFILES"), "tls");

  const lines = readFileSync(path.join(dir, "tokens"), "utf8")
    .split("\n")
    .filter((line) => line && !line.startsWith("#"));
  assert.equal(lines.length, 1);
  const [user, token, role] = lines[0].split(/\s+/);
  assert.equal(user, "alice");
  assert.match(token, /^[0-9a-f]{64}$/);
  assert.equal(role, "admin");

  const key = readFileSync(path.join(dir, "secret.key"), "utf8").trim();
  assert.equal(Buffer.from(key, "base64").length, 32);
  assert.equal(Buffer.from(key, "base64").toString("base64"), key);

  assert.equal(result.stdout.split(token).length - 1, 1, "the token is printed exactly once");
  assert.match(result.stdout, /docker compose up -d/);
  assert.match(result.stdout, /https:\/\/tidebreak\.example\.com/);
  assert.doesNotMatch(result.stdout, new RegExp(env.get("POSTGRES_PASSWORD")));
  assert.doesNotMatch(result.stdout, new RegExp(key.replace(/[+/=]/g, "\\$&")));
});

test("a second run keeps every file byte for byte and prints no token", (t) => {
  const dir = scratch(t);
  const first = run(dir, [
    "--admin",
    "alice",
    "--version",
    minimum,
    "--domain",
    "tidebreak.example.com",
  ]);
  assert.equal(first.status, 0, first.stderr);
  const before = Object.fromEntries(
    [".env", "tokens", "secret.key"].map((name) => [
      name,
      readFileSync(path.join(dir, name), "utf8"),
    ]),
  );

  const again = run(dir, ["--admin", "bob", "--version", "0.118.0", "--domain", ""]);
  assert.equal(again.status, 0, again.stderr);
  for (const [name, content] of Object.entries(before)) {
    assert.equal(readFileSync(path.join(dir, name), "utf8"), content, `${name} changed`);
  }
  assert.match(again.stdout, /Kept, unchanged: \.env tokens secret\.key/);
  assert.doesNotMatch(again.stdout, /Created:|Added to the kept \.env/);
  assert.doesNotMatch(again.stdout, /[0-9a-f]{64}/);
  // The address still comes from the .env that was kept.
  assert.match(again.stdout, /https:\/\/tidebreak\.example\.com/);
});

test("only the missing files are written, and only their inputs are required", (t) => {
  const dir = scratch(t);
  const roster = "# kept\nops 0123456789abcdef0123456789abcdef admin\n";
  writeFileSync(path.join(dir, "tokens"), roster, { mode: 0o600 });

  // No --admin: the tokens file already names one.
  const result = run(dir, ["--version", minimum, "--domain", ""]);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(readFileSync(path.join(dir, "tokens"), "utf8"), roster);
  assert.match(result.stdout, /Created: \.env secret\.key/);
  assert.match(result.stdout, /Kept, unchanged: tokens/);
  assert.doesNotMatch(result.stdout, /Admin token/);
});

test("a kept .env gains the settings the compose file needs, and nothing else changes", (t) => {
  const dir = scratch(t);
  // An .env from before setup.sh existed: no release, no host group, and no
  // newline at the end.
  const old = "POSTGRES_PASSWORD=kept-password\nTIDEBREAK_LOG=debug";
  writeFileSync(path.join(dir, ".env"), old, { mode: 0o600 });

  const result = run(dir, ["--admin", "alice", "--version", "0.118.0"]);
  assert.equal(result.status, 0, result.stderr);
  const content = readFileSync(path.join(dir, ".env"), "utf8");
  assert.ok(content.startsWith(`${old}\n`), content);
  const env = readEnv(dir);
  assert.equal(env.get("POSTGRES_PASSWORD"), "kept-password");
  assert.equal(env.get("TIDEBREAK_LOG"), "debug");
  assert.equal(env.get("TIDEBREAK_VERSION"), "0.118.0");
  assert.match(env.get("TIDEBREAK_HOST_GID"), /^\d+$/);
  assert.match(result.stdout, /Added to the kept \.env: TIDEBREAK_HOST_GID TIDEBREAK_VERSION/);
  assert.doesNotMatch(result.stdout, /Kept, unchanged: \.env/);
  if (linux && !root) {
    // The key created now belongs to the group the compose file will add.
    assert.equal(
      String(statSync(path.join(dir, "secret.key")).gid),
      env.get("TIDEBREAK_HOST_GID"),
    );
  }

  // A later run finds nothing missing.
  const again = run(dir, ["--version", "0.118.0"]);
  assert.equal(again.status, 0, again.stderr);
  assert.doesNotMatch(again.stdout, /Added to the kept \.env/);
});

test("a kept .env that names another group is refused before the key is written", (t) => {
  if (!linux || root) {
    t.skip("file groups apply to a Linux host user");
    return;
  }
  const dir = scratch(t);
  const groups = new Set(
    spawnSync("id", ["-G"], { encoding: "utf8" }).stdout.trim().split(/\s+/),
  );
  let foreign = 4242;
  while (groups.has(String(foreign))) foreign += 1;
  writeFileSync(
    path.join(dir, ".env"),
    `TIDEBREAK_VERSION=${minimum}\nPOSTGRES_PASSWORD=kept\nTIDEBREAK_HOST_GID=${foreign}\n`,
    { mode: 0o600 },
  );
  const result = run(dir, ["--admin", "alice"]);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, new RegExp(`TIDEBREAK_HOST_GID=${foreign}, which is not one of your groups`));
  assert.deepEqual(readdirSync(dir), [".env"]);
});

test("a release older than local-disk storage is refused, and the latest one is checked", (t) => {
  for (const version of ["0.114.0", "v0.116.0", "0.9.99"]) {
    const dir = scratch(t);
    const result = run(dir, ["--admin", "alice", "--version", version, "--domain", ""]);
    assert.notEqual(result.status, 0, `${version} must be refused`);
    assert.match(result.stderr, /cannot keep blobs on local disk/);
    assert.match(result.stderr, new RegExp(`--version ${minimum.replaceAll(".", "\\.")} or later, or --build`));
    assert.deepEqual(readdirSync(dir), []);
  }

  // Without --version, a published release new enough is pulled.
  const current = scratch(t);
  const pulled = run(current, ["--admin", "alice", "--domain", ""], {
    env: fakeCurl(t, "v0.118.2"),
  });
  assert.equal(pulled.status, 0, pulled.stderr);
  assert.equal(readEnv(current).get("TIDEBREAK_VERSION"), "0.118.2");
  assert.equal(readEnv(current).has("COMPOSE_FILE"), false);
});

test("until a release keeps blobs on local disk, the stack builds this checkout", (t) => {
  const dir = scratch(t);
  const result = run(dir, ["--admin", "alice", "--domain", ""], {
    env: fakeCurl(t, "v0.116.0"),
  });
  assert.equal(result.status, 0, result.stderr);
  const env = readEnv(dir);
  assert.equal(env.get("COMPOSE_FILE"), "docker-compose.yml:docker-compose.build.yml");
  // The base file still needs a release to name; this is the one to switch
  // to once it is published.
  assert.equal(env.get("TIDEBREAK_VERSION"), minimum);
  assert.match(result.stdout, /The latest release, 0\.116\.0, cannot keep blobs on local disk/);
  assert.match(result.stdout, /builds the server image from this checkout/);

  // --build chooses the same without asking GitHub.
  const built = scratch(t);
  const explicit = run(built, ["--admin", "alice", "--build", "--domain", ""], {
    env: fakeCurl(t, null),
  });
  assert.equal(explicit.status, 0, explicit.stderr);
  assert.equal(readEnv(built).get("COMPOSE_FILE"), "docker-compose.yml:docker-compose.build.yml");

  // With no network and no choice, the script stops and says what to pass.
  const offline = scratch(t);
  const refused = run(offline, ["--admin", "alice", "--domain", ""], { env: fakeCurl(t, null) });
  assert.notEqual(refused.status, 0);
  assert.match(refused.stderr, /pass --version X\.Y\.Z \(0\.117\.0 or later\), or --build/);
  assert.deepEqual(readdirSync(offline), []);
  assert.notEqual(run(scratch(t), ["--build", "--version", minimum, "--admin", "a"]).status, 0);
});

test("without a domain the stack stays on loopback and Caddy stays off", (t) => {
  const dir = scratch(t);
  const result = run(dir, ["--admin", "alice", "--version", minimum, "--domain", ""]);
  assert.equal(result.status, 0, result.stderr);
  const env = readEnv(dir);
  for (const name of ["TIDEBREAK_DOMAIN", "TIDEBREAK_PUBLIC_URL", "COMPOSE_PROFILES"]) {
    assert.equal(env.has(name), false, `${name} must be unset`);
  }
  assert.match(result.stdout, /http:\/\/127\.0\.0\.1:8080/);
});

test("the next command quotes a directory the shell would split", (t) => {
  const parent = scratch(t);
  const dir = path.join(parent, "tidebreak's deploy");
  mkdirSync(dir);
  const result = run(dir, ["--admin", "alice", "--version", minimum, "--domain", ""], {
    cwd: parent,
  });
  assert.equal(result.status, 0, result.stderr);
  const cd = result.stdout.split("\n").find((line) => line.trim().startsWith("cd "));
  assert.equal(cd.trim(), `cd '${parent}/tidebreak'\\''s deploy'`);
  // The quoted path is the one a shell reads back.
  const echoed = spawnSync("sh", ["-c", `${cd.trim()} && pwd`], { encoding: "utf8" });
  assert.equal(echoed.stdout.trim(), statSync(dir).isDirectory() ? realPath(dir) : dir);
});

function realPath(dir) {
  return spawnSync("sh", ["-c", 'cd "$1" && pwd', "sh", dir], { encoding: "utf8" }).stdout.trim();
}

test("invalid input stops the script before it writes anything", (t) => {
  const current = ["--version", minimum];
  for (const [args, message] of [
    [["--admin", "al!ce", ...current, "--domain", ""], /admin user id/],
    [["--admin", "a".repeat(65), ...current, "--domain", ""], /at most 64/],
    [[...current, "--domain", ""], /--admin USER/],
    [["--admin", "alice", "--version", "latest", "--domain", ""], /release number/],
    [["--admin", "alice", ...current, "--domain", "https://tidebreak.example.com"], /plain host name/],
    [["--admin", "alice", ...current, "--domain", "tidebreak.example.com:8443"], /plain host name/],
    [["--admin", "alice", ...current, "--domain", "tidebreak..example.com"], /plain host name/],
    [["--admin", "alice", ...current, "--domain", ".example.com"], /plain host name/],
    [["--admin", "alice", ...current, "--domain", "-tidebreak.example.com"], /plain host name/],
    [["--admin", "alice", ...current, "--domain", "tidebreak-.example.com"], /plain host name/],
    [["--admin", "alice", ...current, "--domain", `${"a".repeat(64)}.example.com`], /plain host name/],
    [["--admin", "alice", ...current, "--domain", "*.example.com"], /plain host name/],
    [["--admin", "alice", "--bogus"], /unknown option --bogus/],
  ]) {
    const dir = scratch(t);
    const result = run(dir, args);
    assert.notEqual(result.status, 0, `${args.join(" ")} must fail`);
    assert.match(result.stderr, message);
    assert.deepEqual(readdirSync(dir), [], `${args.join(" ")} wrote files`);
  }
  for (const domain of ["localhost", "tidebreak.example.com", "a-b.c1.example"]) {
    const result = run(scratch(t), ["--admin", "alice", ...current, "--domain", domain]);
    assert.equal(result.status, 0, `${domain}: ${result.stderr}`);
  }
});

test("docker-compose.yml reads what setup writes", () => {
  // Each value setup.sh writes to .env is interpolated by the compose file,
  // or reaches the server through `env_file`.
  for (const name of ["TIDEBREAK_VERSION", "POSTGRES_PASSWORD", "TIDEBREAK_HOST_GID", "TIDEBREAK_DOMAIN"]) {
    assert.match(compose, new RegExp(`\\$\\{${name}[:}]`), `compose never reads ${name}`);
  }
  assert.match(compose, /env_file:\n\s+- \.env\n/);
  assert.match(compose, /profiles: \["tls"\]/);
  assert.match(compose, /- "\$\{TIDEBREAK_HOST_GID:-10001\}"/);
  assert.match(compose, /- \.\/tokens:\/run\/tidebreak\/tokens:ro\n/);
  assert.match(compose, /- \.\/secret\.key:\/run\/tidebreak\/secret\.key:ro\n/);
  assert.match(compose, /TIDEBREAK_AUTH_TOKENS_FILE: \/run\/tidebreak\/tokens\n/);
  assert.match(compose, /TIDEBREAK_SECRET_KEY_FILE: \/run\/tidebreak\/secret\.key\n/);
  // Blobs default to the data volume, and the server is published to
  // loopback only.
  assert.match(
    compose,
    /TIDEBREAK_BLOB_STORE_URL: \$\{TIDEBREAK_BLOB_STORE_URL:-file:\/\/\/var\/lib\/tidebreak\/blobs\}/,
  );
  assert.match(compose, /- tidebreak-data:\/var\/lib\/tidebreak\n/);
  assert.match(compose, /- "127\.0\.0\.1:8080:8080"/);
  assert.doesNotMatch(compose, /0\.0\.0\.0:8080:8080/);
  // `up` pulls the release; building is the separate override file's job.
  assert.match(
    compose,
    /image: ghcr\.io\/brightwave-inc\/tidebreak-server:\$\{TIDEBREAK_VERSION:\?/,
  );
  assert.doesNotMatch(compose, /^\s+build:/m);
  // The release setup.sh refuses to go below is the one the compose file's
  // error names.
  const floor = readFileSync(setup, "utf8").match(/^min_version=(\S+)$/m);
  assert.ok(floor, "setup.sh names its minimum release");
  assert.equal(floor[1], minimum);
  assert.match(compose, new RegExp(`TIDEBREAK_VERSION in \\.env \\(${minimum.replaceAll(".", "\\.")} or later`));
});
