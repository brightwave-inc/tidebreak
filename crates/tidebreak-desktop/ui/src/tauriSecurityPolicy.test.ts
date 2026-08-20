import { readFileSync } from "node:fs";
import { expect, it } from "vitest";

it("allows only the approved GitHub avatar hosts in the desktop image policy", () => {
  const config = JSON.parse(
    readFileSync(new URL("../../tauri.conf.json", import.meta.url), "utf8"),
  ) as { app: { security: { csp: string } } };
  const imagePolicy = config.app.security.csp
    .split(";")
    .map((directive) => directive.trim())
    .find((directive) => directive.startsWith("img-src "));

  expect(imagePolicy).toBe(
    "img-src 'self' asset: blob: data: https://github.com https://avatars.githubusercontent.com",
  );
});
