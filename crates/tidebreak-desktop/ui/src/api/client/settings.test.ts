import { afterEach, describe, expect, it, vi } from "vitest";

const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  isTauri: vi.fn(() => true),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: tauri.invoke,
  isTauri: tauri.isTauri,
}));

import type { WorkspaceConfigApplyRequest } from "../types";
import { setAttachedRemotely } from "../../host";
import { HttpCore } from "./http";
import { withSettingsApi } from "./settings";

const Client = withSettingsApi(HttpCore);

const request: WorkspaceConfigApplyRequest = {
  document: {
    tidebreak_config: 1,
    exported_at: "2026-09-02T00:00:00Z",
    sections: { code_repositories: [], mcp_servers: [] },
  },
  decisions: [],
};

afterEach(() => {
  setAttachedRemotely(false);
  tauri.invoke.mockReset();
  vi.unstubAllGlobals();
});

describe("applyWorkspaceConfig", () => {
  it("applies through the native command on this computer", async () => {
    // The native command is what shows the OS dialog before an import
    // starts a local MCP command; the renderer's own route refuses those.
    tauri.invoke.mockResolvedValue({ applied: 0, skipped: 0 });
    const fetch = vi.fn();
    vi.stubGlobal("fetch", fetch);

    await new Client("http://127.0.0.1:1", "token").applyWorkspaceConfig(
      request,
    );

    expect(tauri.invoke).toHaveBeenCalledWith("apply_native_workspace_config", {
      request,
    });
    expect(fetch).not.toHaveBeenCalled();
  });

  it("applies over HTTP while attached to another machine", async () => {
    setAttachedRemotely(true);
    const fetch = vi.fn(
      async () =>
        new Response(JSON.stringify({ applied: 0, skipped: 0 }), {
          status: 200,
        }),
    );
    vi.stubGlobal("fetch", fetch);

    await new Client("https://machine.example", "token").applyWorkspaceConfig(
      request,
    );

    expect(tauri.invoke).not.toHaveBeenCalled();
    expect(fetch).toHaveBeenCalledWith(
      "https://machine.example/workspace-config/apply",
      expect.objectContaining({ method: "POST" }),
    );
  });
});
