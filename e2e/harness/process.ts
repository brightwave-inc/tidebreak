import { execFile } from "node:child_process";
import { promisify } from "node:util";

const run = promisify(execFile);

/** Run a command and return its standard output, failing loudly on a non-zero exit. */
export async function command(
  file: string,
  args: string[],
  options: { cwd?: string; env?: NodeJS.ProcessEnv } = {},
): Promise<string> {
  try {
    const { stdout } = await run(file, args, {
      cwd: options.cwd,
      env: options.env ?? process.env,
      maxBuffer: 16 * 1024 * 1024,
    });
    return stdout;
  } catch (error) {
    const failure = error as { stderr?: string; message: string };
    throw new Error(
      `${file} ${args.join(" ")} failed: ${failure.stderr?.trim() || failure.message}`,
    );
  }
}

/**
 * Wait until `check` holds, asking again every `intervalMs`.
 *
 * This waits on a condition, never on the clock: the deadline only bounds a
 * service that never comes up, and names what it was waiting for.
 */
export async function until(
  what: string,
  check: () => Promise<boolean>,
  { timeoutMs = 60_000, intervalMs = 200 } = {},
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    if (await check().catch(() => false)) return;
    if (Date.now() > deadline) {
      throw new Error(`Timed out after ${timeoutMs} ms waiting for ${what}.`);
    }
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }
}
