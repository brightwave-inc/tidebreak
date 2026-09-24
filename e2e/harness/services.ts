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

export async function startServices(): Promise<Services> {
  const prefix = `tidebreak-e2e-${process.pid}-${randomBytes(3).toString("hex")}`;
  const postgres = `${prefix}-postgres`;
  const storage = `${prefix}-storage`;
  // Minted per run and handed to the containers through the environment, so
  // no credential is ever written into this tree.
  const accessKey = `e2e${randomBytes(8).toString("hex")}`;
  const secretKey = randomBytes(24).toString("hex");
  const started: string[] = [];
  try {
    await command("docker", [
      "run",
      "--detach",
      "--name",
      postgres,
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
    started.push(postgres);
    await command(
      "docker",
      [
        "run",
        "--detach",
        "--name",
        storage,
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
        env: {
          ...process.env,
          ROOT_ACCESS_KEY_ID: accessKey,
          ROOT_SECRET_ACCESS_KEY: secretKey,
        },
      },
    );
    started.push(storage);
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
      await command("docker", [
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
    return services;
  } catch (error) {
    await removeContainers(started);
    throw error;
  }
}

export async function stopServices(services: Services): Promise<void> {
  await removeContainers([
    services.postgres.container,
    services.storage.container,
  ]);
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
  await command("docker", [
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

async function publishedPort(container: string, port: number): Promise<number> {
  const answer = await command("docker", ["port", container, `${port}/tcp`]);
  const match = /127\.0\.0\.1:(\d+)/.exec(answer);
  if (!match)
    throw new Error(
      `${container} published ${port} somewhere unexpected: ${answer}`,
    );
  return Number(match[1]);
}

async function removeContainers(names: string[]): Promise<void> {
  if (names.length === 0) return;
  await command("docker", ["rm", "--force", "--volumes", ...names]).catch(
    (error: Error) =>
      console.warn(`Could not remove ${names.join(", ")}: ${error.message}`),
  );
}
