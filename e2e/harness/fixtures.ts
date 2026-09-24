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
/** A WebSocket to anywhere but this computer's loopback. */
const OFF_HOST_SOCKET = /^(?!wss?:\/\/127\.0\.0\.1[:/])wss?:\/\//i;

/**
 * Server errors a flow may see while an open issue tracks them, keyed by
 * that issue. Delete an issue's entry when it is fixed.
 */
export type KnownServerErrors = {
  [issue: `#${number}`]: {
    method: string;
    /** Matched against the request path, without the query. */
    path: RegExp;
  }[];
};

export const test = base.extend<{
  scripts: MachineScripts;
  knownServerErrors: KnownServerErrors;
  machine: Machine;
}>({
  /** What the machine plays for this flow. Set per file with `test.use`. */
  scripts: [{}, { option: true }],

  /** Server errors this flow tolerates. Set per file with `test.use`. */
  knownServerErrors: [{}, { option: true }],

  /**
   * The browser context, held to the host and to a healthy server. A request
   * or WebSocket that would leave this computer is refused, and so is any
   * 5xx the machine answers the page with: either fails the flow, so a flow
   * can never pass on something the network answered or on an error the
   * page swallowed. The context depends on the machine so that it closes
   * first: the page never watches its server shut down.
   */
  context: async ({ context, knownServerErrors, machine }, use) => {
    const offHost: string[] = [];
    await context.route(OFF_HOST, (route) => {
      offHost.push(route.request().url());
      return route.abort("blockedbyclient");
    });
    await context.routeWebSocket(OFF_HOST_SOCKET, (socket) => {
      offHost.push(socket.url());
      socket.close({
        code: 1008,
        reason: "The end-to-end lane stays on this host.",
      });
    });
    const serverErrors: string[] = [];
    const origin = new URL(machine.url).origin;
    context.on("response", (response) => {
      if (response.status() < 500) return;
      const url = new URL(response.url());
      if (url.origin !== origin) return;
      const method = response.request().method();
      const known = Object.values(knownServerErrors)
        .flat()
        .some(
          (entry) => entry.method === method && entry.path.test(url.pathname),
        );
      if (!known) {
        serverErrors.push(`${response.status()} ${method} ${url.pathname}`);
      }
    });
    await use(context);
    expect(
      { offHost, serverErrors },
      "requests that left the host, and server errors the page got",
    ).toEqual({ offHost: [], serverErrors: [] });
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
