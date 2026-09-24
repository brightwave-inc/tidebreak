import { registerRepository } from "../harness/api";
import { expect, openApp, test } from "../harness/fixtures";
import { createFixtureRepository } from "../harness/repository";
import { codeTurn } from "../harness/scripts";

test.use({
  scripts: {
    harness: codeTurn({
      reply: "Added hello.txt.",
      writes: [
        { path: "hello.txt", contents: "hello from the scripted engine\n" },
      ],
    }),
  },
});

test("a code turn shows its reply and the diff it made", async ({
  page,
  machine,
}) => {
  await registerRepository(
    machine,
    await createFixtureRepository(machine.root),
  );
  await openApp(page, machine);
  await page.getByRole("radio", { name: "Code" }).click();

  await page.getByRole("button", { name: "New workspace on fixture" }).click();
  const dialog = page.getByRole("dialog", { name: "New workspace" });
  await dialog
    .getByRole("textbox", { name: "First message" })
    .fill("Add a hello file");
  await dialog.getByRole("button", { name: "Create" }).click();

  // The first message shows as soon as the session accepts it, and the
  // engine's reply follows on the live stream.
  const main = page.getByRole("main");
  await expect(main.getByRole("article", { name: "You" })).toContainText(
    "Add a hello file",
  );
  await expect(main.getByRole("article", { name: "Assistant" })).toContainText(
    "Added hello.txt.",
  );
  const turn = main.getByRole("group", { name: "Turn finished" });
  await expect(
    turn.getByRole("button", { name: "Review this turn's changes" }),
  ).toContainText("1 file");

  await turn
    .getByRole("button", { name: "Review this turn's changes" })
    .click();
  const diff = main.getByRole("tabpanel", { name: "Turn diff" });
  await expect(diff.getByRole("heading", { name: "hello.txt" })).toBeVisible();
  await expect(diff).toContainText("+hello from the scripted engine");
  await expect(page.getByRole("tab", { name: /Source control/ })).toContainText(
    "1",
  );
});
