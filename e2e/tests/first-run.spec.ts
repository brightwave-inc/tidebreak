import { randomBytes } from "node:crypto";

import { expect, test } from "../harness/fixtures";
import { serveModelList } from "../harness/model-endpoint";

const ANSWER = "Welcome aboard. Ask me anything about this machine.";

test.use({ scripts: { provider: [{ text: ANSWER }] } });

test("a new browser signs in, connects a model, and gets its first answer", async ({
  page,
  machine,
}) => {
  const endpoint = await serveModelList("e2e-model");
  try {
    // The machine's address alone signs nobody in.
    await page.goto(machine.url);
    await expect(
      page.getByRole("heading", { name: "Sign in to this machine" }),
    ).toBeVisible();
    await page.getByLabel("Token").fill(randomBytes(32).toString("hex"));
    await page.getByRole("button", { name: "Sign in" }).click();
    await expect(page.getByRole("alert")).toHaveText(
      /This machine refused that token/,
    );
    await page.getByLabel("Token").fill(machine.token);
    await page.getByRole("button", { name: "Sign in" }).click();
    await expect(
      page.getByRole("heading", { name: "Start with a repository" }),
    ).toBeVisible();

    // No model yet, so Work says what to do instead of letting a message go.
    await page.getByRole("radio", { name: "Work" }).click();
    await expect(
      page.getByRole("heading", { name: "Connect a model to start" }),
    ).toBeVisible();
    await expect(
      page.getByText("Connect a model provider to send."),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Send message" }),
    ).toBeDisabled();

    await page
      .getByRole("button", { name: /Connect an OpenAI-compatible server/ })
      .click();
    await expect(
      page.getByRole("heading", { name: "Providers" }),
    ).toBeVisible();
    await page
      .getByRole("textbox", { name: "Base URL" })
      .fill(endpoint.baseUrl);
    await page.getByRole("button", { name: "Save configuration" }).click();
    await expect(
      page.getByRole("status").filter({ hasText: "Connected" }),
    ).toContainText("It lists 1 model.");
    await page.getByRole("button", { name: "Find models" }).click();
    const found = page.getByRole("dialog", {
      name: "Find OpenAI-compatible models",
    });
    await found.getByRole("checkbox", { name: "Add e2e-model" }).check();
    await found.getByRole("button", { name: "Review 1 model" }).click();
    await page
      .getByRole("dialog", { name: "Check 1 model before adding" })
      .getByRole("button", { name: "Add 1 model" })
      .click();
    await expect(
      page.getByRole("list", { name: "OpenAI-compatible custom models" }),
    ).toContainText("e2e-model");

    await page.getByRole("button", { name: /Back to app/ }).click();
    await expect(
      page.getByRole("heading", { name: "Welcome to Tidebreak" }),
    ).toBeVisible();
    await page.getByRole("textbox", { name: "Message" }).fill("Hello, machine");
    await page.getByRole("button", { name: "Send message" }).click();
    const main = page.getByRole("main");
    await expect(main.getByRole("article", { name: "You" })).toContainText(
      "Hello, machine",
    );
    await expect(
      main.getByRole("article", { name: "Assistant" }),
    ).toContainText(ANSWER);
  } finally {
    await endpoint.close();
  }
});
