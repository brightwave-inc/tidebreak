// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { AssistantSource } from "./AssistantSources";
import { MessageCitationsProvider } from "./InlineCitation";
import { MarkdownLinkProvider, MessageMarkdown } from "./MessageMarkdown";

const openInBrowser = vi.hoisted(() => vi.fn());
vi.mock("./openInBrowser", () => ({
  openInBrowser: (...args: unknown[]) => openInBrowser(...args),
}));

afterEach(() => {
  cleanup();
  openInBrowser.mockReset();
});

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

describe("table copy", () => {
  it("copies the rendered cells as tab-separated rows", async () => {
    const user = userEvent.setup();
    render(
      <MessageMarkdown>
        {"| Name | Value |\n| --- | --- |\n| Alpha | **1** |"}
      </MessageMarkdown>,
    );
    await user.click(screen.getByRole("button", { name: "Copy table" }));
    expect(await window.navigator.clipboard.readText()).toBe(
      "Name\tValue\nAlpha\t1",
    );
  });
});

const DOCUMENT = "0b2b1f2c-9d3e-4a5b-8c7d-6e5f4a3b2c1d";
const OLD_CITATION = "3f7c8a91-2b4d-4e6f-9a1b-5c7d8e9f0a1b";

const source: AssistantSource = {
  id: "citation-1",
  ordinal: 1,
  documentId: DOCUMENT,
  locator: { kind: "lines", start: 12, end: 18 },
};

describe("inline citations", () => {
  it("opens the model-authored document locator from its cited phrase", async () => {
    const user = userEvent.setup();
    const onOpenSource = vi.fn();
    const { container } = render(
      <MessageCitationsProvider value={{ sources: [source], onOpenSource }}>
        <MessageMarkdown>
          {`The reef :cit[is the largest *in the world*]{doc=${DOCUMENT} lines=12-18}.`}
        </MessageMarkdown>
      </MessageCitationsProvider>,
    );

    const cited = screen.getByRole("button", {
      name: "is the largest in the world, citation 1",
    });
    expect(container.textContent).not.toContain(":cit");
    await user.click(cited);
    expect(onOpenSource).toHaveBeenCalledWith(source);
  });

  it("leaves a locator as prose until its durable snapshot arrives", () => {
    const { container } = render(
      <MessageMarkdown>
        {`The reef :cit[grows slowly]{doc=${DOCUMENT} page=4}.`}
      </MessageMarkdown>,
    );
    expect(screen.queryByRole("button")).toBeNull();
    expect(container).toHaveTextContent("The reef grows slowly.");
  });

  it("degrades historical citation-id directives to bare cited text", () => {
    const { container } = render(
      <MessageMarkdown>
        {`The reef :cit[grows slowly]{citation_id=${OLD_CITATION}}.`}
      </MessageMarkdown>,
    );
    expect(screen.queryByRole("button")).toBeNull();
    expect(container).toHaveTextContent("The reef grows slowly.");
    expect(container.textContent).not.toContain("citation_id");
  });
});

describe("markdown links", () => {
  const LOCAL =
    "Storybook is still at http://127.0.0.1:6031/?path=/story/code-workspace-card--hover-idle-session if you want another look.";

  it("keeps a localhost URL as one link after a model line wrap", () => {
    render(
      <MessageMarkdown>
        {
          "Storybook is still at http://127.0.0.1:6031/?\npath=/story/code-workspace-card--hover-idle-session if you want another look."
        }
      </MessageMarkdown>,
    );
    const link = screen.getByRole("link");
    expect(link).toHaveAttribute(
      "href",
      "http://127.0.0.1:6031/?path=/story/code-workspace-card--hover-idle-session",
    );
    expect(link).toHaveTextContent(
      "http://127.0.0.1:6031/?path=/story/code-workspace-card--hover-idle-session",
    );
  });

  it("opens the in-app browser on click, and the system browser on a command click", async () => {
    const user = userEvent.setup();
    const onOpenInApp = vi.fn();
    render(
      <MarkdownLinkProvider onOpenInApp={onOpenInApp}>
        <MessageMarkdown>{LOCAL}</MessageMarkdown>
      </MarkdownLinkProvider>,
    );
    const link = screen.getByRole("link");
    await user.click(link);
    expect(onOpenInApp).toHaveBeenCalledWith(
      "http://127.0.0.1:6031/?path=/story/code-workspace-card--hover-idle-session",
    );
    expect(openInBrowser).not.toHaveBeenCalled();

    onOpenInApp.mockClear();
    const command = /Mac|iPhone|iPad|iPod/.test(navigator.userAgent);
    fireEvent.click(link, { metaKey: command, ctrlKey: !command });
    expect(onOpenInApp).not.toHaveBeenCalled();
    expect(openInBrowser).toHaveBeenCalledWith(
      "http://127.0.0.1:6031/?path=/story/code-workspace-card--hover-idle-session",
    );
  });
});
