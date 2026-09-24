import { randomBytes } from "node:crypto";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { test as base, expect, type Page } from "@playwright/test";

import { type Machine, type MachineScripts, startMachine } from "./machine";
import { publishedServices } from "./services";

export { expect };

/** Anything the page asks for that is not on this computer's loopback. */
const OFF_HOST = /^(?!http:\/\/127\.0\.0\.1[:/])[a-z][a-z0-9+.-]*:\/\//i;

export const test = base.extend<{ scripts: MachineScripts; machine: Machine }>({
  /** What the machine plays for this flow. Set per file with `test.use`. */
  scripts: [{}, { option: true }],

  /**
   * The browser context, held to the host: a request that would leave this
   * computer is refused and fails the flow, so a flow can never pass on
   * something the network happened to answer.
   */
  context: async ({ context }, use) => {
    const refused: string[] = [];
    await context.route(OFF_HOST, (route) => {
      refused.push(route.request().url());
      return route.abort("blockedbyclient");
    });
    await use(context);
    expect(refused, "requests that tried to leave the host").toEqual([]);
  },

  /** A fresh machine for every flow: its own data directory, database, and bucket prefix. */
  machine: async ({ scripts }, use, testInfo) => {
    const root = mkdtempSync(join(tmpdir(), "tidebreak-e2e-"));
    const name = `flow_${randomBytes(6).toString("hex")}`;
    let machine: Machine | undefined;
    try {
      machine = await startMachine({
        services: publishedServices(),
        root,
        name,
        scripts,
      });
      await use(machine);
    } finally {
      await machine?.stop();
      // The server's side of a failure travels with the trace, including a
      // machine that never came up.
      if (!machine || testInfo.status !== testInfo.expectedStatus) {
        for (const [label, path] of [
          ["server output", join(root, "server.log")],
          ["server log", join(root, "data", "logs", "tidebreak.log")],
        ]) {
          if (existsSync(path))
            await testInfo.attach(label, { path, contentType: "text/plain" });
        }
      }
      rmSync(root, { recursive: true, force: true });
    }
  },
});

/**
 * Open the machine's page already holding the administrator's bearer.
 *
 * The bearer rides the fragment, the one carrier the page accepts
 * (decision 82), and the page takes it out of the address before its router
 * starts.
 */
export async function openApp(page: Page, machine: Machine): Promise<void> {
  await page.goto(`${machine.url}/#handoff=${machine.token}`);
  await expect(
    page.getByRole("radiogroup", { name: "App mode" }),
  ).toBeVisible();
}
