import { execFileSync } from "node:child_process";
import { randomBytes } from "node:crypto";

import { command, until } from "./process";

/**
 * The self-host profile's two backing services, one of each per lane run.
 *
 * The profile stores rows in PostgreSQL and blob bytes in S3-compatible
 * storage, and refuses to boot without either. Both run in throwaway
 * containers bound to loopback, pinned by digest like every image CI runs.
 * Each machine gets its own database and its own bucket prefix, so flows never
 * see each other's state.
 */
export type Services = {
  postgres: { container: string; port: number };
  storage: {
    container: string;
    port: number;
    bucket: string;
    accessKey: string;
    secretKey: string;
  };
};

/** The same image the CI `postgres state machine` lane runs. */
const POSTGRES_IMAGE =
  "postgres:17-alpine@sha256:742f40ea20b9ff2ff31db5458d127452988a2164df9e17441e191f3b72252193";
/** Versity's S3 gateway over a plain directory: conditional writes and multipart copy included. */
const STORAGE_IMAGE =
  "versity/versitygw:v1.8.0@sha256:30292fc2eeacc67a36993b01f7a7a5e3361a19cced0e80c1d71cfa2a4b0a2499";
const BUCKET = "tidebreak-e2e";
const SERVICES_VARIABLE = "TIDEBREAK_E2E_SERVICES";
/**
 * Every container this lane starts carries this label, set to the process
 * that owns it, so a later run can find the ones a killed run left behind.
 */
const OWNER_LABEL = "io.tidebreak.e2e.owner";
/** Bounds for the Docker calls, so a registry or daemon that stalls fails the run instead of holding it. */
const PULL_TIMEOUT_MS = 180_000;
const DOCKER_TIMEOUT_MS = 60_000;

/**
 * Containers this process started and has not removed yet.
 *
 * The global teardown removes them on an ordinary finish. Everything else
 * works from this record too: a signal removes them at once, and the
 * process's exit hook removes whatever is left, which covers a run whose
 * teardown Playwright skipped after a global timeout. Only a SIGKILL gets
 * past all three, and the next run's sweep covers that.
 */
const owned = new Set<string>();

export async function startServices(): Promise<Services> {
  const prefix = `tidebreak-e2e-${process.pid}-${randomBytes(3).toString("hex")}`;
  const postgres = `${prefix}-postgres`;
  const storage = `${prefix}-storage`;
  // Minted per run and handed to the containers through the environment, so
  // no credential is ever written into this tree.
  const accessKey = `e2e${randomBytes(8).toString("hex")}`;
  const secretKey = randomBytes(24).toString("hex");
  await Promise.all([ensureImage(POSTGRES_IMAGE), ensureImage(STORAGE_IMAGE)]);
  process.once("exit", () => removeContainersNow([...owned]));
  // While the containers start no teardown exists yet, so SIGINT needs the
  // same handling as SIGTERM.
  const disarmStartup = removeOnSignal(["SIGINT", "SIGTERM"]);
  try {
    // Recorded before the run call returns, so a signal that lands while the
    // container starts still removes it.
    owned.add(postgres);
    await docker([
      "run",
      "--detach",
      "--pull",
      "never",
      "--name",
      postgres,
      "--label",
      `${OWNER_LABEL}=${process.pid}`,
      "--publish",
      "127.0.0.1::5432",
      "--tmpfs",
      "/var/lib/postgresql/data",
      "--env",
      "POSTGRES_HOST_AUTH_METHOD=trust",
      POSTGRES_IMAGE,
      "postgres",
      "-c",
      "fsync=off",
      "-c",
      "synchronous_commit=off",
    ]);
    owned.add(storage);
    await docker(
      [
        "run",
        "--detach",
        "--pull",
        "never",
        "--name",
        storage,
        "--label",
        `${OWNER_LABEL}=${process.pid}`,
        "--publish",
        "127.0.0.1::7070",
        "--env",
        "ROOT_ACCESS_KEY_ID",
        "--env",
        "ROOT_SECRET_ACCESS_KEY",
        "--entrypoint",
        "sh",
        STORAGE_IMAGE,
        "-c",
        `mkdir -p /data/${BUCKET} && exec versitygw --quiet posix /data`,
      ],
      {
        ...process.env,
        ROOT_ACCESS_KEY_ID: accessKey,
        ROOT_SECRET_ACCESS_KEY: secretKey,
      },
    );
    const services: Services = {
      postgres: {
        container: postgres,
        port: await publishedPort(postgres, 5432),
      },
      storage: {
        container: storage,
        port: await publishedPort(storage, 7070),
        bucket: BUCKET,
        accessKey,
        secretKey,
      },
    };
    // The image's init runs a socket-only server first, so a TCP answer
    // means the real one is up.
    await until("PostgreSQL to accept connections", async () => {
      await docker([
        "exec",
        postgres,
        "pg_isready",
        "--host",
        "127.0.0.1",
        "--username",
        "postgres",
      ]);
      return true;
    });
    await until("the S3 gateway to answer", async () => {
      await fetch(`http://127.0.0.1:${services.storage.port}/`);
      return true;
    });
    // From here Playwright's own SIGINT stop runs the teardown; SIGTERM
    // still needs this handler.
    disarmStartup();
    removeOnSignal(["SIGTERM"]);
    return services;
  } catch (error) {
    disarmStartup();
    await removeContainers([...owned]);
    throw error;
  }
}

export async function stopServices(services: Services): Promise<void> {
  await removeContainers([
    services.postgres.container,
    services.storage.container,
  ]);
}

/**
 * Remove the containers a run left behind when it was killed before its
 * teardown ran. A container is stale once the process that owns it is gone,
 * so a run going on in another checkout keeps its own.
 */
export async function removeStaleContainers(): Promise<void> {
  const listing = await docker([
    "ps",
    "--all",
    "--filter",
    `label=${OWNER_LABEL}`,
    "--format",
    `{{.Names}}\t{{.Label "${OWNER_LABEL}"}}`,
  ]);
  const stale = listing
    .split("\n")
    .map((line) => line.trim().split("\t"))
    .filter(([name, owner]) => name && !processIsAlive(Number(owner)))
    .map(([name]) => name);
  if (stale.length > 0) {
    console.warn(
      `Removing containers a killed run left behind: ${stale.join(", ")}`,
    );
    await removeContainers(stale);
  }
}

/** Hand the services to the worker processes, which inherit this environment. */
export function publishServices(services: Services): void {
  process.env[SERVICES_VARIABLE] = JSON.stringify(services);
}

export function publishedServices(): Services {
  const value = process.env[SERVICES_VARIABLE];
  if (!value)
    throw new Error("The lane's global setup did not start its services.");
  return JSON.parse(value) as Services;
}

/** A fresh database for one machine. */
export async function createDatabase(
  services: Services,
  name: string,
): Promise<string> {
  await docker([
    "exec",
    services.postgres.container,
    "createdb",
    "--host",
    "127.0.0.1",
    "--username",
    "postgres",
    name,
  ]);
  return `postgres://postgres@127.0.0.1:${services.postgres.port}/${name}`;
}

/**
 * Remove this process's containers at once when it is told to stop by one of
 * `signals`, and return a function that disarms the handler.
 *
 * Playwright stops gracefully on SIGINT once the run is under way, and its
 * global teardown removes the containers. It gives SIGTERM no such handling.
 * So the containers go first, synchronously, and a SIGTERM then becomes the
 * SIGINT stop, so the running flow still ends and reports.
 */
function removeOnSignal(signals: readonly NodeJS.Signals[]): () => void {
  const disarm = () => {
    for (const signal of signals) process.off(signal, onSignal);
  };
  function onSignal(signal: NodeJS.Signals) {
    disarm();
    removeContainersNow([...owned]);
    // With no other listener, nothing else will stop the run: stop it here.
    if (process.listenerCount("SIGINT") === 0) {
      process.exit(signal === "SIGINT" ? 130 : 143);
    }
    if (signal === "SIGTERM") process.kill(process.pid, "SIGINT");
  }
  for (const signal of signals) process.on(signal, onSignal);
  return disarm;
}

async function ensureImage(image: string): Promise<void> {
  try {
    await docker(["image", "inspect", "--format", "{{.Id}}", image]);
  } catch {
    await command("docker", ["pull", "--quiet", image], {
      timeoutMs: PULL_TIMEOUT_MS,
    });
  }
}

async function publishedPort(container: string, port: number): Promise<number> {
  const answer = await docker(["port", container, `${port}/tcp`]);
  const match = /127\.0\.0\.1:(\d+)/.exec(answer);
  if (!match)
    throw new Error(
      `${container} published ${port} somewhere unexpected: ${answer}`,
    );
  return Number(match[1]);
}

function docker(args: string[], env?: NodeJS.ProcessEnv): Promise<string> {
  return command("docker", args, { env, timeoutMs: DOCKER_TIMEOUT_MS });
}

async function removeContainers(names: readonly string[]): Promise<void> {
  if (names.length === 0) return;
  await docker(["rm", "--force", "--volumes", ...names]).then(
    () => {
      for (const name of names) owned.delete(name);
    },
    (error: Error) => {
      if (/No such container/i.test(error.message)) {
        for (const name of names) owned.delete(name);
      } else {
        console.warn(`Could not remove ${names.join(", ")}: ${error.message}`);
      }
    },
  );
}

function removeContainersNow(names: readonly string[]): void {
  if (names.length === 0) return;
  try {
    execFileSync("docker", ["rm", "--force", "--volumes", ...names], {
      stdio: "ignore",
      timeout: DOCKER_TIMEOUT_MS,
    });
    for (const name of names) owned.delete(name);
  } catch {
    // Best effort while the process is going down; the next run's sweep
    // removes anything left.
  }
}

function processIsAlive(pid: number): boolean {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    // EPERM: the process exists but belongs to someone else.
    return (error as NodeJS.ErrnoException).code === "EPERM";
  }
}
