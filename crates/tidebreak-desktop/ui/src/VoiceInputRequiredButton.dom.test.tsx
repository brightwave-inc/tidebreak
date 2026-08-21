// @vitest-environment jsdom
import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { renderWithRouter } from "./test/router";
import { VoiceInputRequiredButton } from "./VoiceInputRequiredButton";

describe("voice input settings routing", () => {
  it("takes an unavailable voice model directly to voice input settings", async () => {
    const user = userEvent.setup();
    const { router } = await renderWithRouter(<VoiceInputRequiredButton />);

    await user.click(
      screen.getByRole("button", { name: "Configure voice input" }),
    );

    await waitFor(() => {
      expect(router.state.location.pathname).toBe(
        "/settings/voice-transcription",
      );
    });
  });
});
