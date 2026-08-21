import { readFileSync } from "node:fs";
import { expect, it } from "vitest";

function directive(name: string): string | undefined {
  const config = JSON.parse(
    readFileSync(new URL("../../tauri.conf.json", import.meta.url), "utf8"),
  ) as { app: { security: { csp: string } } };
  return config.app.security.csp
    .split(";")
    .map((entry) => entry.trim())
    .find((entry) => entry.startsWith(`${name} `));
}

it("allows only the approved GitHub avatar hosts in the desktop image policy", () => {
  expect(directive("img-src")).toBe(
    "img-src 'self' asset: blob: data: https://github.com https://avatars.githubusercontent.com",
  );
});

it("lets the connection policy reach a remote machine over TLS", () => {
  // The policy is compiled into the binary, and the remote machine is whatever
  // address the operator attaches, so the scheme is the narrowest form this
  // can take. Losing either token blocks every request to a remote machine.
  const connectPolicy = directive("connect-src");

  expect(connectPolicy).toContain(" https:");
  expect(connectPolicy).toContain(" wss:");
});
