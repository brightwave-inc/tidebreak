import { connectScriptedModel } from "../harness/api";
import { expect, openApp, test } from "../harness/fixtures";

const ANSWER = "High tide comes about every twelve hours.";

test.use({ scripts: { provider: [{ text: ANSWER }] } });

test("a new chat streams its answer and keeps it across a reload", async ({
  page,
  machine,
}) => {
  await connectScriptedModel(machine);
  await openApp(page, machine);
  await page.getByRole("radio", { name: "Work" }).click();

  await page
    .getByRole("textbox", { name: "Message" })
    .fill("When is high tide?");
  await page.getByRole("button", { name: "Send message" }).click();

  // Nothing reloads here: the answer can only arrive over the live event stream.
  const main = page.getByRole("main");
  await expect(main.getByRole("article", { name: "You" })).toContainText(
    "When is high tide?",
  );
  await expect(main.getByRole("article", { name: "Assistant" })).toContainText(
    ANSWER,
  );
  await expect(
    main.getByRole("status").filter({ hasText: "Ready to send" }),
  ).toBeVisible();
  await expect(
    page
      .getByRole("group", { name: "Today" })
      .getByRole("button", { name: /^New conversation/ }),
  ).toBeVisible();

  // The tab holds its bearer in memory alone, so a reload signs in again, and
  // signing in lands on the conversation the address still names.
  const conversation = page.url();
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "Sign in to this machine" }),
  ).toBeVisible();
  await page.getByLabel("Token").fill(machine.token);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(main.getByRole("article", { name: "Assistant" })).toContainText(
    ANSWER,
  );
  expect(page.url()).toBe(conversation);
});
