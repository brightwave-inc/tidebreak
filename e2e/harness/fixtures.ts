import { randomBytes } from "node:crypto";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { test as base, expect, type Page } from "@playwright/test";

import { type Machine, type MachineScripts, startMachine } from "./machine";
import { publishedServices } from "./services";

export { expect };

export const test = base.extend<{ scripts: MachineScripts; machine: Machine }>({
  /** What the machine plays for this flow. Set per file with `test.use`. */
  scripts: [{}, { option: true }],

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
      if (testInfo.status !== testInfo.expectedStatus) {
        // The server's side of a failure travels with the trace.
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
 * (decision 82); the page takes it out of the address before its router
 * starts. `route` lands the page on a hash route once it has signed in.
 */
export async function openApp(
  page: Page,
  machine: Machine,
  route?: string,
): Promise<void> {
  const handoff = new URLSearchParams({ handoff: machine.token });
  if (route) handoff.set("return_to", route);
  await page.goto(`${machine.url}/#${handoff.toString()}`);
  await expect(
    page.getByRole("radiogroup", { name: "App mode" }),
  ).toBeVisible();
}
