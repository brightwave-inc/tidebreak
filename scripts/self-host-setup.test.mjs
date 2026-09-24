import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
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
// files it writes, their modes, and that it never replaces one that exists.

const setup = fileURLToPath(new URL("../deploy/self-host/setup.sh", import.meta.url));
const compose = readFileSync(
  new URL("../deploy/self-host/docker-compose.yml", import.meta.url),
  "utf8",
);

// On Linux the server's uid reads tokens and secret.key through the owner's
// group, which docker-compose.yml adds to the container. Docker Desktop and
// OrbStack on macOS share files with any container uid, so there they stay
// owner-only.
const sharedMode = process.platform === "darwin" ? 0o600 : 0o640;

function scratch(t) {
  const dir = mkdtempSync(path.join(tmpdir(), "tidebreak-setup-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

function run(dir, ...args) {
  return spawnSync("sh", [setup, "--dir", dir, ...args], {
    encoding: "utf8",
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
  const result = run(
    dir,
    "--admin",
    "alice",
    "--version",
    "v0.116.0",
    "--domain",
    "tidebreak.example.com",
  );
  assert.equal(result.status, 0, result.stderr);

  assert.equal(mode(path.join(dir, ".env")), 0o600);
  assert.equal(mode(path.join(dir, "tokens")), sharedMode);
  assert.equal(mode(path.join(dir, "secret.key")), sharedMode);
  if (process.platform !== "darwin") {
    // The group the files carry is the one .env tells compose to add.
    assert.equal(statSync(path.join(dir, "tokens")).gid, process.getgid());
    assert.equal(statSync(path.join(dir, "secret.key")).gid, process.getgid());
  }

  const env = readEnv(dir);
  assert.equal(env.get("TIDEBREAK_VERSION"), "0.116.0");
  assert.match(env.get("POSTGRES_PASSWORD"), /^[0-9a-f]{64}$/);
  assert.equal(env.get("TIDEBREAK_HOST_GID"), String(process.getgid()));
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
  assert.equal(
    run(dir, "--admin", "alice", "--version", "0.116.0", "--domain", "tidebreak.example.com")
      .status,
    0,
  );
  const before = Object.fromEntries(
    [".env", "tokens", "secret.key"].map((name) => [
      name,
      readFileSync(path.join(dir, name), "utf8"),
    ]),
  );

  const again = run(dir, "--admin", "bob", "--version", "0.117.0", "--domain", "");
  assert.equal(again.status, 0, again.stderr);
  for (const [name, content] of Object.entries(before)) {
    assert.equal(readFileSync(path.join(dir, name), "utf8"), content, `${name} changed`);
  }
  assert.match(again.stdout, /Kept, unchanged: \.env tokens secret\.key/);
  assert.doesNotMatch(again.stdout, /Created:/);
  assert.doesNotMatch(again.stdout, /[0-9a-f]{64}/);
  // The address still comes from the .env that was kept.
  assert.match(again.stdout, /https:\/\/tidebreak\.example\.com/);
});

test("only the missing files are written, and only their inputs are required", (t) => {
  const dir = scratch(t);
  const roster = "# kept\nops 0123456789abcdef0123456789abcdef admin\n";
  writeFileSync(path.join(dir, "tokens"), roster, { mode: 0o600 });

  // No --admin: the tokens file already names one.
  const result = run(dir, "--version", "0.116.0", "--domain", "");
  assert.equal(result.status, 0, result.stderr);
  assert.equal(readFileSync(path.join(dir, "tokens"), "utf8"), roster);
  assert.match(result.stdout, /Created: \.env secret\.key/);
  assert.match(result.stdout, /Kept, unchanged: tokens/);
  assert.doesNotMatch(result.stdout, /Admin token/);
});

test("without a domain the stack stays on loopback and Caddy stays off", (t) => {
  const dir = scratch(t);
  const result = run(dir, "--admin", "alice", "--version", "0.116.0", "--domain", "");
  assert.equal(result.status, 0, result.stderr);
  const env = readEnv(dir);
  for (const name of ["TIDEBREAK_DOMAIN", "TIDEBREAK_PUBLIC_URL", "COMPOSE_PROFILES"]) {
    assert.equal(env.has(name), false, `${name} must be unset`);
  }
  assert.match(result.stdout, /http:\/\/127\.0\.0\.1:8080/);
});

test("invalid input stops the script before it writes anything", (t) => {
  for (const [args, message] of [
    [["--admin", "al!ce", "--version", "0.116.0", "--domain", ""], /admin user id/],
    [["--admin", "a".repeat(65), "--version", "0.116.0", "--domain", ""], /at most 64/],
    [["--version", "0.116.0", "--domain", ""], /--admin USER/],
    [["--admin", "alice", "--version", "latest", "--domain", ""], /release number/],
    [
      ["--admin", "alice", "--version", "0.116.0", "--domain", "https://tidebreak.example.com"],
      /plain host name/,
    ],
    [["--admin", "alice", "--version", "0.116.0", "--domain", "tidebreak.example.com:8443"], /plain host name/],
    [["--admin", "alice", "--bogus"], /unknown option --bogus/],
  ]) {
    const dir = scratch(t);
    const result = run(dir, ...args);
    assert.notEqual(result.status, 0, `${args.join(" ")} must fail`);
    assert.match(result.stderr, message);
    assert.deepEqual(readdirSync(dir), [], `${args.join(" ")} wrote files`);
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
});
