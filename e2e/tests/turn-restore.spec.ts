import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

import { registerRepository } from "../harness/api";
import { startWorkspace, workspaceCheckouts } from "../harness/code";
import { expect, openApp, test } from "../harness/fixtures";
import { createFixtureRepository } from "../harness/repository";
import { codeTurn } from "../harness/scripts";

const CONTENTS = "hello from the scripted engine\n";

test.use({
  scripts: {
    harness: codeTurn({
      reply: "Added hello.txt.",
      writes: [{ path: "hello.txt", contents: CONTENTS }],
    }),
  },
});

test("a turn's output is removed by a restore and comes back on undo", async ({
  page,
  machine,
}) => {
  await registerRepository(
    machine,
    await createFixtureRepository(machine.root),
  );
  await openApp(page, machine);
  await startWorkspace(page, "Add a hello file");

  const main = page.getByRole("main");
  const files = page.getByRole("tree", { name: "Workspace files" });
  const [checkout] = workspaceCheckouts(machine);
  const output = join(checkout, "hello.txt");
  await expect(
    files.getByRole("treeitem", { name: "hello.txt" }),
  ).toBeVisible();
  expect(readFileSync(output, "utf8")).toBe(CONTENTS);

  await main.getByRole("button", { name: "Turn actions" }).click();
  await page
    .getByRole("menuitem", { name: "Restore to before this turn" })
    .click();
  const confirm = page.getByRole("alertdialog", {
    name: "Restore to before this turn?",
  });
  await expect(confirm).toContainText("hello.txt");
  await confirm.getByRole("button", { name: "Restore" }).click();

  const restored = main.getByRole("group", {
    name: "Restored to before turn 1",
  });
  await expect(restored).toBeVisible();
  await expect(
    main.getByRole("button", { name: "Workspace status: No changes" }),
  ).toBeVisible();
  await expect(files.getByRole("treeitem", { name: "hello.txt" })).toHaveCount(
    0,
  );
  expect(existsSync(output)).toBe(false);

  await restored.getByRole("button", { name: "Undo the restore" }).click();
  const undo = page.getByRole("alertdialog", { name: "Undo this restore?" });
  await expect(undo).toContainText("hello.txt");
  await undo.getByRole("button", { name: "Undo restore" }).click();
  await expect(
    files.getByRole("treeitem", { name: "hello.txt" }),
  ).toBeVisible();
  await expect(
    main.getByRole("button", { name: "Workspace status: 1 changed file" }),
  ).toBeVisible();
  expect(readFileSync(output, "utf8")).toBe(CONTENTS);
});
