// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { PluginCatalog } from "@/api";
import { WelcomeState, type PromptLibraryApis } from "./WelcomeState";

const EMPTY_CATALOG: PluginCatalog = { plugins: [], skills: [], prompts: [] };

const CATALOG: PluginCatalog = {
  ...EMPTY_CATALOG,
  prompts: [
    {
      name: "weekly-update",
      description: "Draft this week's status note.",
      origin: "user",
      plugin: null,
      enabled: true,
    },
    {
      name: "retired-brief",
      description: "From a bundle that is switched off.",
      origin: "builtin",
      plugin: "reporting",
      enabled: false,
    },
  ],
};

function libraryWith(catalog: PluginCatalog): PromptLibraryApis {
  return {
    list: vi.fn().mockResolvedValue(catalog),
    promptBody: vi
      .fn()
      .mockResolvedValue({ name: "weekly-update", body: "Write my weekly update covering " }),
  };
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("WelcomeState starters", () => {
  it("offers the enabled library prompts and inserts the picked body", async () => {
    const library = libraryWith(CATALOG);
    const onSelectPrompt = vi.fn();
    render(
      <WelcomeState onSelectPrompt={onSelectPrompt} promptLibrary={library} />,
    );

    const card = await screen.findByRole("button", { name: /Weekly update/ });
    expect(screen.getByText("Draft this week's status note.")).toBeInTheDocument();
    // A prompt whose bundle is off is not on offer, and the static openers
    // step aside once the library has something to say.
    expect(screen.queryByText(/Retired brief/)).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "What can you help me with?" }),
    ).not.toBeInTheDocument();

    fireEvent.click(card);
    await waitFor(() =>
      expect(onSelectPrompt).toHaveBeenCalledWith(
        "Write my weekly update covering ",
      ),
    );
    expect(library.promptBody).toHaveBeenCalledWith("weekly-update");
  });

  it("keeps the built-in starters when the library has nothing enabled", async () => {
    const library = libraryWith(EMPTY_CATALOG);
    const onSelectPrompt = vi.fn();
    render(
      <WelcomeState onSelectPrompt={onSelectPrompt} promptLibrary={library} />,
    );

    // Shown from the first paint, not after the catalog answers: an install
    // with no prompts must never see home flicker or empty out.
    const starter = screen.getByRole("button", {
      name: "What can you help me with?",
    });
    await waitFor(() => expect(library.list).toHaveBeenCalled());
    fireEvent.click(starter);
    expect(onSelectPrompt).toHaveBeenCalledWith("What can you help me with?");
  });
});
