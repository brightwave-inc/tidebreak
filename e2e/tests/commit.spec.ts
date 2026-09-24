import { registerRepository } from "../harness/api";
import { startWorkspace, workspaceCheckouts } from "../harness/code";
import { expect, openApp, test } from "../harness/fixtures";
import {
  committedFile,
  createFixtureRepository,
  latestCommit,
} from "../harness/repository";
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

test("a commit from source control lands on the workspace branch", async ({
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
  const surfaces = page.getByRole("complementary", {
    name: "Workspace surfaces",
  });
  await surfaces.getByRole("tab", { name: /Source control/ }).click();
  await expect(
    surfaces.getByRole("list", { name: "Changed files" }),
  ).toContainText("hello.txt");
  await expect(surfaces.getByText("Commits 1 changed file.")).toBeVisible();

  await surfaces
    .getByRole("textbox", { name: "Commit message" })
    .fill("Add the hello file");
  await surfaces.getByRole("button", { name: "Commit", exact: true }).click();

  await expect(surfaces.getByText("No changes to commit.")).toBeVisible();
  await expect(
    main.getByRole("button", { name: "Workspace status: Unpushed commits" }),
  ).toBeVisible();
  // The commit on the workspace branch carries the engine's file, and
  // nothing is left behind uncommitted.
  const [checkout] = workspaceCheckouts(machine);
  expect(await latestCommit(checkout, machine.root)).toEqual({
    subject: "Add the hello file",
    changes: ["A\thello.txt"],
    clean: true,
  });
  expect(await committedFile(checkout, machine.root, "hello.txt")).toBe(
    CONTENTS,
  );
});
