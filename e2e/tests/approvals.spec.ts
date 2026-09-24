import { existsSync, readdirSync } from "node:fs";
import { join } from "node:path";

import { connectScriptedModel } from "../harness/api";
import { expect, openApp, test } from "../harness/fixtures";
import type { Machine } from "../harness/machine";
import { writeFile } from "../harness/scripts";

test.use({
  scripts: {
    // Two turns of two steps each: ask to write a file, then answer.
    provider: [
      writeFile("notes.md", "# Notes\n\nKeep these.\n"),
      { text: "Saved notes.md." },
      writeFile("draft.md", "# Draft\n\nDo not keep this.\n"),
      { text: "Left draft.md alone." },
    ],
  },
});

test("a file write waits for approval, runs when allowed, and stops when denied", async ({
  page,
  machine,
}) => {
  await connectScriptedModel(machine);
  await openApp(page, machine);
  await page.getByRole("radio", { name: "Work" }).click();
  const main = page.getByRole("main");
  const composer = page.getByRole("textbox", { name: "Message" });
  const approval = main.getByRole("region", { name: "Approval needed" });

  await composer.fill("Save my notes");
  await page.getByRole("button", { name: "Send message" }).click();
  await expect(
    approval.getByRole("heading", { name: "Write this file?" }),
  ).toBeVisible();
  await expect(approval).toContainText("notes.md");
  await expect(
    main.getByRole("status").filter({ hasText: "Waiting for your approval" }),
  ).toBeVisible();
  await approval.getByRole("button", { name: "1. Yes, allow it once" }).click();
  await expect(approval).toHaveCount(0);
  await expect(
    main.getByRole("article", { name: "Assistant" }).last(),
  ).toContainText("Saved notes.md.");

  await composer.fill("Save a draft too");
  await page.getByRole("button", { name: "Send message" }).click();
  await expect(approval).toContainText("draft.md");
  await approval
    .getByRole("button", { name: "2. No, don't allow this" })
    .click();
  await expect(approval).toHaveCount(0);
  await expect(
    main.getByRole("article", { name: "Assistant" }).last(),
  ).toContainText("Left draft.md alone.");
  await expect(
    main.getByRole("status").filter({ hasText: "Ready to send" }),
  ).toBeVisible();

  // The allowed call wrote its file; the denied one kept its untensed title
  // and wrote nothing, in the page and on the machine's disk.
  await main
    .getByRole("button", { name: "Updated a file", exact: true })
    .click();
  await expect(
    main
      .getByRole("listitem")
      .filter({ hasText: /notes\.md\s*21\s*B/ })
      .last(),
  ).toBeVisible();
  await main
    .getByRole("button", { name: "Update a file", exact: true })
    .click();
  await expect(
    main.getByRole("listitem").filter({ hasText: "draft.md" }).first(),
  ).toBeVisible();
  await expect(
    main.getByRole("listitem").filter({ hasText: /draft\.md\s*\d+\s*B/ }),
  ).toHaveCount(0);
  expect(scratchFiles(machine)).toContain("notes.md");
  expect(scratchFiles(machine)).not.toContain("draft.md");
});

/** The files in every conversation workspace on the machine. */
function scratchFiles(machine: Machine): string[] {
  const scratch = join(machine.dataDir, "scratch");
  if (!existsSync(scratch)) return [];
  return readdirSync(scratch, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .flatMap((entry) => readdirSync(join(scratch, entry.name)));
}
