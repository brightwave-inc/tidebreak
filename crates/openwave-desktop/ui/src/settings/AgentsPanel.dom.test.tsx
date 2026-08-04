// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient, RuntimeSettings } from "../api";
import { AgentsPanel } from "./AgentsPanel";

afterEach(cleanup);

describe("AgentsPanel", () => {
  it("loads and saves the per-chat active background-agent cap", async () => {
    const settings: RuntimeSettings = {
      model: null,
      has_api_key: false,
      chat_defaults: {
        model: null,
        reasoning_effort: null,
        permission_mode: null,
        network_policy: null,
      },
      max_active_background_agents: 5,
    };
    const putSettings = vi.fn().mockResolvedValue({
      ...settings,
      max_active_background_agents: 8,
    });
    const client = {
      getSettings: vi.fn().mockResolvedValue(settings),
      putSettings,
    } as unknown as ApiClient;

    render(<AgentsPanel client={client} />);
    await screen.findByText("Active background agents per chat");
    const input = screen.getByRole("spinbutton");
    expect(input).toHaveValue(5);
    fireEvent.change(input, { target: { value: "8" } });
    fireEvent.click(screen.getByRole("button", { name: "Save settings" }));
    await waitFor(() =>
      expect(putSettings).toHaveBeenCalledWith({
        max_active_background_agents: 8,
      }),
    );
  });
});
