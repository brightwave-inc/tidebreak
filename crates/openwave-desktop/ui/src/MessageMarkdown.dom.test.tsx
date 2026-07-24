// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import { MessageMarkdown } from "./MessageMarkdown";

afterEach(cleanup);

describe("code block copy", () => {
  it("copies the raw source, not the highlighted markup", async () => {
    const user = userEvent.setup();
    render(
      <MessageMarkdown>{"```ts\nconst x: number = 1;\n```"}</MessageMarkdown>,
    );
    await user.click(screen.getByRole("button", { name: "Copy code" }));
    expect(await window.navigator.clipboard.readText()).toBe(
      "const x: number = 1;\n",
    );
  });
});
