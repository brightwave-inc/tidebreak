// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { ToolOutputPreview } from "./ToolOutputPreview";

const twelveLines = Array.from(
  { length: 12 },
  (_, index) => `line ${index + 1}`,
).join("\n");

afterEach(cleanup);

describe("ToolOutputPreview", () => {
  it("clamps long output and gives the rest back on request", async () => {
    render(<ToolOutputPreview text={twelveLines} collapsedLines={8} />);

    const body = screen.getByLabelText("Output");
    expect(body.textContent).toContain("line 8");
    expect(body.textContent).not.toContain("line 9");

    await userEvent.click(
      screen.getByRole("button", { name: "Show 4 more lines" }),
    );

    expect(screen.getByLabelText("Output").textContent).toContain("line 12");
    expect(screen.getByRole("button", { name: "Show less" })).toBeTruthy();
  });

  it("leaves short output whole, with the copy control still available", () => {
    render(<ToolOutputPreview text={"one\ntwo"} />);

    expect(screen.getByLabelText("Output").textContent).toBe("one\ntwo");
    expect(screen.queryByRole("button", { name: /more line/ })).toBeNull();
    expect(screen.getByRole("button", { name: "Copy output" })).toBeTruthy();
  });
});
