// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { ShortcutsList } from "./ShortcutsDialog";

afterEach(cleanup);

describe("ShortcutsList", () => {
  it("lists the diff's file keys in code mode", () => {
    render(<ShortcutsList mode="code" command />);
    // J and K as on GitHub, or ] and [ as on GitLab.
    const keycaps = (description: string) =>
      [
        ...(screen
          .getByText(description)
          .nextElementSibling?.querySelectorAll("kbd") ?? []),
      ].map((cap) => cap.textContent);
    expect(keycaps("Next file")).toEqual(["J", "]"]);
    expect(keycaps("Previous file")).toEqual(["K", "["]);
    expect(
      screen.getByText("Comment on the selected lines"),
    ).toBeInTheDocument();
  });

  it("leaves them out of chat, which has no diff", () => {
    render(<ShortcutsList mode="chat" command={false} />);
    expect(screen.queryByText("Next file")).toBeNull();
  });
});
