// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AppInvokeRefusalError, type AppDetail, type AppGrantState } from "@/api";
import { AppDetailView } from "./AppDetailView";
import type { AppsApis } from "./appsApis";

const DETAIL: AppDetail = {
  id: "app-1",
  name: "Fixture app",
  created_at: "2026-07-01T00:00:00Z",
  updated_at: "2026-07-02T00:00:00Z",
  current_revision: "rev-2",
  revisions: [
    { id: "rev-2", ordinal: 2, created_at: "2026-07-02T00:00:00Z" },
    { id: "rev-1", ordinal: 1, created_at: "2026-07-01T00:00:00Z" },
  ],
};

const GRANTED: AppGrantState = {
  granted: true,
  bindings: [
    {
      app: "11111111-1111-4111-8111-111111111111",
      name: "cmd",
      tools: ["mcp__cmd__doit"],
      operation_ids: null,
      granted: true,
      definition_changed: false,
    },
  ],
};

const STALE: AppGrantState = {
  granted: false,
  bindings: [
    {
      app: "11111111-1111-4111-8111-111111111111",
      name: "cmd",
      tools: ["mcp__cmd__doit"],
      operation_ids: null,
      granted: false,
      definition_changed: true,
    },
    {
      app: "22222222-2222-4222-8222-222222222222",
      name: "issues",
      tools: null,
      operation_ids: ["listIssues"],
      granted: false,
      definition_changed: false,
    },
  ],
};

function apisWith(grant: AppGrantState): AppsApis {
  return {
    baseUrl: "http://127.0.0.1:7777",
    list: vi.fn().mockResolvedValue({ apps: [] }),
    get: vi.fn().mockResolvedValue(DETAIL),
    deleteApp: vi.fn().mockResolvedValue(undefined),
    grantState: vi.fn().mockResolvedValue(grant),
    consent: vi.fn().mockResolvedValue(GRANTED),
    revoke: vi.fn().mockResolvedValue(undefined),
    viewSession: vi
      .fn()
      .mockResolvedValue({ frame_path: "/apps/view-frames/token-1" }),
    invoke: vi
      .fn()
      .mockResolvedValue({
        content: '{"ok":true}',
        structured_content: { ok: true },
        is_error: false,
      }),
    invokeOperation: vi.fn().mockResolvedValue({
      status: 200,
      content_type: "application/json",
      body_base64: "e30=",
      is_error: false,
    }),
  };
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("AppDetailView", () => {
  it("opens a granted app and drives one invoke round trip through the bridge", async () => {
    const apis = apisWith(GRANTED);
    render(<AppDetailView appId="app-1" apis={apis} onBack={() => {}} />);

    // Granted: no sheet, the sandboxed frame mounts at the single-use address.
    const frame = (await screen.findByTitle(
      "App: Fixture app",
    )) as HTMLIFrameElement;
    expect(apis.viewSession).toHaveBeenCalledWith("app-1");
    expect(frame).toHaveAttribute(
      "src",
      "http://127.0.0.1:7777/apps/view-frames/token-1",
    );
    expect(frame).toHaveAttribute("sandbox", "allow-scripts");
    expect(frame.getAttribute("sandbox")).not.toContain("allow-same-origin");

    // The frame asks for a tool call; the parent forwards it to the invoke
    // route and posts the result back — both directions opaque passthrough.
    const contentWindow = frame.contentWindow!;
    const posted = vi.spyOn(contentWindow, "postMessage");
    window.dispatchEvent(
      new MessageEvent("message", {
        data: {
          jsonrpc: "2.0",
          id: 3,
          method: "tools/call",
          params: { name: "mcp__cmd__doit", arguments: { q: 1 } },
        },
        source: contentWindow,
      }),
    );
    await waitFor(() => expect(posted).toHaveBeenCalled());
    expect(apis.invoke).toHaveBeenCalledWith("app-1", "mcp__cmd__doit", {
      q: 1,
    });
    expect(posted).toHaveBeenCalledWith(
      {
        jsonrpc: "2.0",
        id: 3,
        result: {
          content: [{ type: "text", text: '{"ok":true}' }],
          structuredContent: { ok: true },
          isError: false,
        },
      },
      "*",
    );
  });

  it("drives one REST operation round trip through the bridge", async () => {
    const apis = apisWith(GRANTED);
    render(<AppDetailView appId="app-1" apis={apis} onBack={() => {}} />);

    const frame = (await screen.findByTitle(
      "App: Fixture app",
    )) as HTMLIFrameElement;
    const contentWindow = frame.contentWindow!;
    const posted = vi.spyOn(contentWindow, "postMessage");

    // The frame asks for a REST operation; the parent forwards it to the
    // same invoke route and posts the REST result back verbatim.
    window.dispatchEvent(
      new MessageEvent("message", {
        data: {
          jsonrpc: "2.0",
          id: 4,
          method: "operations/call",
          params: {
            operation_id: "listIssues",
            parameters: { state: "open" },
            body: { page: 2 },
          },
        },
        source: contentWindow,
      }),
    );
    await waitFor(() => expect(posted).toHaveBeenCalled());
    expect(apis.invokeOperation).toHaveBeenCalledWith(
      "app-1",
      "listIssues",
      { state: "open" },
      { page: 2 },
    );
    expect(posted).toHaveBeenCalledWith(
      {
        jsonrpc: "2.0",
        id: 4,
        result: {
          status: 200,
          content_type: "application/json",
          body_base64: "e30=",
          is_error: false,
        },
      },
      "*",
    );
  });

  it("re-gates behind the sheet when an operation call refuses with consent_required", async () => {
    const apis = apisWith(GRANTED);
    apis.grantState = vi
      .fn()
      .mockResolvedValueOnce(GRANTED)
      .mockResolvedValue(STALE);
    apis.invokeOperation = vi
      .fn()
      .mockRejectedValue(
        new AppInvokeRefusalError("consent_required", "Consent required"),
      );
    render(<AppDetailView appId="app-1" apis={apis} onBack={() => {}} />);

    const frame = (await screen.findByTitle(
      "App: Fixture app",
    )) as HTMLIFrameElement;
    const contentWindow = frame.contentWindow!;
    const posted = vi.spyOn(contentWindow, "postMessage");
    window.dispatchEvent(
      new MessageEvent("message", {
        data: {
          jsonrpc: "2.0",
          id: 5,
          method: "operations/call",
          params: { operation_id: "listIssues" },
        },
        source: contentWindow,
      }),
    );

    // The frame sees a typed JSON-RPC error; the host refetches the grant
    // and drops the frame back behind the consent sheet.
    await waitFor(() =>
      expect(posted).toHaveBeenCalledWith(
        {
          jsonrpc: "2.0",
          id: 5,
          error: {
            code: -32000,
            message: "Consent required",
            data: { kind: "consent_required" },
          },
        },
        "*",
      ),
    );
    expect(
      await screen.findByRole("button", { name: "Allow access" }),
    ).toBeInTheDocument();
    expect(screen.queryByTitle(/^App:/)).not.toBeInTheDocument();
  });

  it("gates an ungranted app behind the sheet, marks staleness, and consents body-less", async () => {
    const apis = apisWith(STALE);
    render(<AppDetailView appId="app-1" apis={apis} onBack={() => {}} />);

    // The sheet renders the server projection: the connected app's display
    // name and tools, and the marker for a
    // definition that changed since the previous consent.
    expect(await screen.findByText("mcp__cmd__doit")).toBeInTheDocument();
    // A rest_api binding renders its operation ids in the same list.
    expect(screen.getByText("listIssues")).toBeInTheDocument();
    expect(
      screen.getByText("Reconfigured since you agreed"),
    ).toBeInTheDocument();
    expect(screen.queryByTitle(/^App:/)).not.toBeInTheDocument();

    // Consent is a bare affirmative — the client method carries only the app
    // id, so a stale sheet can never author what gets granted — and the
    // server's fresh verdict is what opens the frame.
    fireEvent.click(screen.getByRole("button", { name: "Allow access" }));
    expect(apis.consent).toHaveBeenCalledWith("app-1");
    expect(await screen.findByTitle("App: Fixture app")).toBeInTheDocument();
  });
});
