import { type ChildProcess, spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import {
  createWriteStream,
  mkdirSync,
  type WriteStream,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { createInterface } from "node:readline";

import { rendererBundle, serverBinary } from "./paths";
import { codeTurn, type HarnessScript, type ProviderStep } from "./scripts";
import { createDatabase, type Services } from "./services";

export type MachineScripts = {
  provider?: ProviderStep[];
  harness?: HarnessScript;
};

/** One self-host machine: the debug server with its own data, database, and bucket prefix. */
export type Machine = {
  /** The machine's origin, which also serves the renderer. */
  url: string;
  /** An administrator's bearer from the machine's token file. */
  token: string;
  /** Scratch the machine owns: its data directory, home, and logs. */
  root: string;
  dataDir: string;
  stop(): Promise<void>;
};

/** What a machine plays when a flow does not script it: a plain answer, and a plain engine turn. */
const DEFAULT_PROVIDER: ProviderStep[] = [{ text: "Scripted answer." }];
const DEFAULT_HARNESS = codeTurn({ reply: "Scripted engine reply." });

export async function startMachine({
  services,
  root,
  name,
  scripts,
}: {
  services: Services;
  root: string;
  name: string;
  scripts: MachineScripts;
}): Promise<Machine> {
  const dataDir = join(root, "data");
  const home = join(root, "home");
  mkdirSync(dataDir, { recursive: true });
  mkdirSync(home, { recursive: true });
  const databaseUrl = await createDatabase(services, name);
  const token = randomBytes(32).toString("hex");
  const tokens = join(root, "tokens");
  writeFileSync(tokens, `e2e ${token} admin\n`, { mode: 0o600 });

  // Built from nothing rather than inherited, so no provider key, forge
  // token, or profile variable from the developer's shell reaches the
  // machine. HOME points into scratch for the same reason.
  const env: NodeJS.ProcessEnv = {
    PATH: process.env.PATH,
    TMPDIR: process.env.TMPDIR,
    USER: process.env.USER,
    HOME: home,
    LANG: "C.UTF-8",
    TIDEBREAK_PROFILE: "self_host",
    TIDEBREAK_DATA_DIR: dataDir,
    TIDEBREAK_DATABASE_URL: databaseUrl,
    TIDEBREAK_BLOB_STORE_URL: `s3://${services.storage.bucket}/${name}`,
    TIDEBREAK_AUTH_TOKENS_FILE: tokens,
    TIDEBREAK_UI_DIST: rendererBundle(),
    TIDEBREAK_SCRIPTED_PROVIDER: JSON.stringify(
      scripts.provider ?? DEFAULT_PROVIDER,
    ),
    TIDEBREAK_SCRIPTED_HARNESS: JSON.stringify(
      scripts.harness ?? DEFAULT_HARNESS,
    ),
    // Self-host never opens the OS keychain; the mock makes sure nothing in a
    // debug build can reach the developer's.
    TIDEBREAK_KEYCHAIN_MOCK: "1",
    TIDEBREAK_LOG: "warn",
    AWS_ACCESS_KEY_ID: services.storage.accessKey,
    AWS_SECRET_ACCESS_KEY: services.storage.secretKey,
    AWS_REGION: "us-east-1",
    AWS_ENDPOINT: `http://127.0.0.1:${services.storage.port}`,
    AWS_ALLOW_HTTP: "true",
    AWS_VIRTUAL_HOSTED_STYLE_REQUEST: "false",
    // Commits need an identity, and the system config must not lend one a
    // credential helper such as the macOS keychain.
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_TERMINAL_PROMPT: "0",
    GIT_AUTHOR_NAME: "Tidebreak E2E",
    GIT_AUTHOR_EMAIL: "e2e@example.invalid",
    GIT_COMMITTER_NAME: "Tidebreak E2E",
    GIT_COMMITTER_EMAIL: "e2e@example.invalid",
  };
  const serverLog = join(root, "server.log");
  const log = createWriteStream(serverLog);
  const child = spawn(serverBinary(), ["serve"], {
    cwd: root,
    env,
    stdio: ["ignore", "pipe", "pipe"],
  });
  // `close` fires once the process is gone and its output is drained, which
  // is when the log can end.
  const closed = new Promise<void>((resolve) =>
    child.once("close", () => resolve()),
  );
  child.stderr?.pipe(log, { end: false });
  const stop = () => stopServer(child, closed, log);
  let url: string;
  try {
    url = await listeningUrl(child, log);
  } catch (error) {
    await stop();
    throw new Error(`${(error as Error).message} See ${serverLog}.`);
  }
  return { url, token, root, dataDir, stop };
}

/**
 * Read stdout until the server announces its address, which it prints only
 * once bound. Migrating a fresh database takes seconds; the deadline only
 * catches a server that will never listen.
 */
function listeningUrl(child: ChildProcess, log: WriteStream): Promise<string> {
  return new Promise((resolve, reject) => {
    const deadline = setTimeout(
      () => reject(new Error("tidebreak serve did not listen within 60 s.")),
      60_000,
    );
    const lines = createInterface({ input: child.stdout! });
    lines.on("line", (line) => {
      if (!log.writableEnded) log.write(`${line}\n`);
      const match = /^tidebreak: listening on (http:\/\/\S+)$/.exec(line);
      if (match) {
        clearTimeout(deadline);
        resolve(match[1]);
      }
    });
    child.once("error", (error) => {
      clearTimeout(deadline);
      reject(error);
    });
    child.once("exit", (code, signal) => {
      clearTimeout(deadline);
      reject(
        new Error(
          `tidebreak serve exited (${signal ?? code}) before it listened.`,
        ),
      );
    });
  });
}

async function stopServer(
  child: ChildProcess,
  closed: Promise<void>,
  log: WriteStream,
): Promise<void> {
  if (child.pid === undefined) {
    // It never started, so there is nothing to stop or drain.
  } else if (child.exitCode === null && child.signalCode === null) {
    child.kill("SIGTERM");
    const escalate = setTimeout(() => child.kill("SIGKILL"), 10_000);
    await closed;
    clearTimeout(escalate);
  } else {
    await closed;
  }
  await new Promise<void>((resolve) => log.end(resolve));
}
