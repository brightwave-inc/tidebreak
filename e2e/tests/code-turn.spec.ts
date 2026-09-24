import { registerRepository } from "../harness/api";
import { expect, openApp, test } from "../harness/fixtures";
import { onePixelPng, pasteImage } from "../harness/images";
import { createFixtureRepository } from "../harness/repository";
import { codeTurn } from "../harness/scripts";

/**
 * How long the engine holds the turn before it answers. The first message
 * must reach the screen well inside it, so the flow sees the conversation
 * while the turn is still running rather than after it lands.
 */
const HOLD_MS = 8_000;
const FIRST_MESSAGE_WITHIN_MS = 5_000;

test.use({
  scripts: {
    harness: codeTurn({
      reply: "Added hello.txt.",
      writes: [
        { path: "hello.txt", contents: "hello from the scripted engine\n" },
      ],
      holdMs: HOLD_MS,
    }),
  },
});

test("a first message with an image shows while its turn runs, then the reply and diff", async ({
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
  const message = dialog.getByRole("textbox", { name: "First message" });
  await pasteImage(message, onePixelPng());
  // The dialog holds the image until the workspace exists, so nothing is
  // uploading yet and the chip must not say so.
  const attached = dialog.getByRole("list", { name: "Attached images" });
  await expect(attached.getByRole("listitem")).toContainText("screenshot.png");
  await expect(
    attached.getByRole("button", { name: "Remove screenshot.png" }),
  ).toBeVisible();
  await expect(dialog.getByText(/^Uploading/)).toHaveCount(0);
  await expect(dialog.getByRole("progressbar")).toHaveCount(0);
  await message.fill("Add a hello file");
  await dialog.getByRole("button", { name: "Create" }).click();

  // The conversation shows once the session accepts the first message, not
  // once the engine answers: the message and its image are on screen while
  // the turn is still running and before any reply.
  const main = page.getByRole("main");
  const first = main.getByRole("article", { name: "You" });
  await expect(first).toContainText("Add a hello file", {
    timeout: FIRST_MESSAGE_WITHIN_MS,
  });
  await expect(
    first.getByRole("button", { name: "Expand attached image 1" }),
  ).toBeVisible();
  await expect(
    main.getByRole("button", { name: "Stop response" }),
  ).toBeVisible();
  await expect(main.getByRole("article", { name: "Assistant" })).toHaveCount(0);

  // The engine's reply follows on the live stream.
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
  await expect(diff).toContainText(/This turn\s*1 file\s*\+1\s*−0/);
  await expect(diff.getByRole("heading", { name: "hello.txt" })).toBeVisible();
  await expect(
    diff.getByRole("button", {
      name: "Revert the change at line 1 of hello.txt",
    }),
  ).toBeVisible();
  await expect(diff).toContainText("hello from the scripted engine");
  await expect(page.getByRole("tab", { name: /Source control/ })).toContainText(
    "1",
  );
});
