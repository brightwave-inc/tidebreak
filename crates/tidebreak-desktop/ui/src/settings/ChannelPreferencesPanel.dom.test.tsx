// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, HarnessKind } from "../api";
import { ChannelPreferencesPanel } from "./ChannelPreferencesPanel";
import { parseChannelPreferences } from "./channelPreferences";
const preferences = {
  harness: null as HarnessKind | null,
  model: null,
  respond_automatically: true,
  instructions: "Be brief.",
  channel_id: "C1",
  workspace_identity: "T1",
  settings_path: "/settings/channels",
  can_edit: true,
};
function client() {
  return {
    getChannelPreferences: vi.fn(async () => preferences),
    setChannelPreferences: vi.fn(async (_grant, _channel, value) => ({
      ...preferences,
      ...value,
    })),
    getHarnessDoctor: vi.fn(async () => ({ harnesses: [] })),
    listModels: vi.fn(async () => ({
      models: [
        {
          key: "model_gateway::tools",
          display_name: "Tools model",
          available: true,
          supports_tools: true,
        },
        {
          key: "model_gateway::unavailable",
          display_name: "Unavailable",
          available: false,
          supports_tools: true,
        },
        {
          key: "model_gateway::chat",
          display_name: "Chat only",
          available: true,
          supports_tools: false,
        },
      ],
      roles: [],
    })),
    listCodeHarnessModels: vi.fn(async () => ({ models: [] })),
  };
}
afterEach(cleanup);
describe("Channel preferences", () => {
  it("saves thread reply behavior without changing harness or instructions", async () => {
    const api = client();
    const user = userEvent.setup();
    render(
      <ChannelPreferencesPanel
        client={api as unknown as ApiClient}
        grantId="grant"
        channelId="C1"
      />,
    );
    await user.click(
      await screen.findByRole("switch", { name: "Respond automatically" }),
    );
    await waitFor(() =>
      expect(api.setChannelPreferences).toHaveBeenCalledWith("grant", "C1", {
        harness: null,
        model: null,
        instructions: "Be brief.",
        respond_automatically: false,
      }),
    );
  });
  it("saves instructions on blur and prevents read-only edits", async () => {
    const api = client();
    const user = userEvent.setup();
    const view = render(
      <ChannelPreferencesPanel
        client={api as unknown as ApiClient}
        grantId="grant"
        channelId="C1"
      />,
    );
    const field = await screen.findByRole("textbox", {
      name: "Channel instructions",
    });
    await user.clear(field);
    await user.type(field, "Link to runbooks.");
    await user.tab();
    await waitFor(() =>
      expect(api.setChannelPreferences).toHaveBeenCalledWith("grant", "C1", {
        harness: null,
        model: null,
        respond_automatically: true,
        instructions: "Link to runbooks.",
      }),
    );
    view.unmount();
    api.getChannelPreferences.mockResolvedValue({
      ...preferences,
      can_edit: false,
    });
    render(
      <ChannelPreferencesPanel
        client={api as unknown as ApiClient}
        grantId="grant"
        channelId="C1"
      />,
    );
    expect(
      (
        (await screen.findByRole("textbox", {
          name: "Channel instructions",
        })) as HTMLTextAreaElement
      ).disabled,
    ).toBe(true);
  });
  it("loads internal models from the chat catalog and saves the qualified key", async () => {
    const api = client();
    api.getChannelPreferences.mockResolvedValue({
      ...preferences,
      harness: "internal",
    });
    const user = userEvent.setup();
    render(
      <ChannelPreferencesPanel
        client={api as unknown as ApiClient}
        grantId="grant"
        channelId="C1"
      />,
    );
    await waitFor(() => expect(api.listModels).toHaveBeenCalled());
    expect(api.listCodeHarnessModels).not.toHaveBeenCalled();
    await user.click(await screen.findByRole("combobox", { name: "Model" }));
    expect(screen.queryByRole("option", { name: "Unavailable" })).toBeNull();
    expect(screen.queryByRole("option", { name: "Chat only" })).toBeNull();
    await user.click(
      await screen.findByRole("option", { name: "Tools model" }),
    );
    await waitFor(() =>
      expect(api.setChannelPreferences).toHaveBeenCalledWith(
        "grant",
        "C1",
        expect.objectContaining({ model: "model_gateway::tools" }),
      ),
    );
  });
  it("rejects malformed settings responses", () => {
    expect(parseChannelPreferences(preferences)).toEqual(preferences);
    expect(
      parseChannelPreferences({ ...preferences, respond_automatically: "yes" }),
    ).toBeNull();
  });
});
