import { createReadStream, mkdirSync, statSync, writeFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { join, normalize, sep } from "node:path";

import { command } from "./process";

/** Git with no system or global config, so the developer's settings never leak into a fixture. */
async function git(
  cwd: string,
  home: string,
  ...args: string[]
): Promise<string> {
  return await command("git", args, {
    cwd,
    env: {
      PATH: process.env.PATH,
      HOME: home,
      GIT_CONFIG_NOSYSTEM: "1",
      GIT_TERMINAL_PROMPT: "0",
      GIT_AUTHOR_NAME: "Tidebreak E2E",
      GIT_AUTHOR_EMAIL: "e2e@example.invalid",
      GIT_COMMITTER_NAME: "Tidebreak E2E",
      GIT_COMMITTER_EMAIL: "e2e@example.invalid",
    },
  });
}

/**
 * A small repository on the machine's disk: one commit on `main` with a
 * README, tracking an `origin` that is a bare copy beside it.
 *
 * Returns the checkout's path. The code flows register it the way a desktop
 * registers a local folder. The local origin lets a new workspace bring
 * `main` up to date without leaving the host.
 */
export async function createFixtureRepository(
  root: string,
  name = "fixture",
): Promise<string> {
  const home = join(root, "git-home");
  const checkout = join(root, "repos", name);
  const origin = join(root, "origins", `${name}.git`);
  mkdirSync(home, { recursive: true });
  mkdirSync(checkout, { recursive: true });
  mkdirSync(origin, { recursive: true });
  await git(checkout, home, "init", "--quiet", "--initial-branch", "main");
  writeFileSync(
    join(checkout, "README.md"),
    "# Fixture\n\nA repository for the end-to-end lane.\n",
  );
  await git(checkout, home, "add", "README.md");
  await git(checkout, home, "commit", "--quiet", "--message", "Initial commit");
  await git(
    origin,
    home,
    "init",
    "--quiet",
    "--bare",
    "--initial-branch",
    "main",
  );
  await git(checkout, home, "remote", "add", "origin", origin);
  await git(
    checkout,
    home,
    "push",
    "--quiet",
    "--set-upstream",
    "origin",
    "main",
  );
  return checkout;
}

/** The latest commit subject on a branch of a checkout. */
export async function latestCommitSubject(
  checkout: string,
  root: string,
  ref = "HEAD",
): Promise<string> {
  return (
    await git(checkout, join(root, "git-home"), "log", "-1", "--format=%s", ref)
  ).trim();
}

/**
 * Serve a bare copy of a checkout over git's dumb HTTP protocol on loopback.
 *
 * A browser attached to a machine clones by URL (it cannot name a path on the
 * machine), and a static file server is all the dumb protocol needs once
 * `update-server-info` has written its index. Nothing leaves the host.
 */
export async function serveRepository(
  root: string,
  checkout: string,
): Promise<{ url: string; close(): Promise<void> }> {
  const served = join(root, "served");
  const name = "fixture.git";
  const home = join(root, "git-home");
  mkdirSync(served, { recursive: true });
  await git(served, home, "clone", "--quiet", "--bare", checkout, name);
  await git(join(served, name), home, "update-server-info");
  const server = createServer((request, response) => {
    const path = normalize(
      decodeURIComponent(
        new URL(request.url ?? "/", "http://fixture").pathname,
      ),
    );
    const file = join(served, path);
    if (!file.startsWith(served + sep) || request.method !== "GET") {
      response.writeHead(404).end();
      return;
    }
    try {
      if (!statSync(file).isFile()) throw new Error("not a file");
    } catch {
      response.writeHead(404).end();
      return;
    }
    response.writeHead(200, { "content-type": "application/octet-stream" });
    createReadStream(file).pipe(response);
  });
  const port = await listen(server);
  return {
    url: `http://127.0.0.1:${port}/${name}`,
    close: () => new Promise((resolve) => server.close(() => resolve())),
  };
}

export function listen(server: Server): Promise<number> {
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (address && typeof address === "object") resolve(address.port);
      else reject(new Error("the server has no port"));
    });
  });
}
