// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
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
      app: "22222222-2222-4222-8222-222222222222",
      folder: null,
      gateway_app: null,
      access: null,
      name: "issues",
      operation_ids: ["listIssues"],
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
      folder: null,
      gateway_app: null,
      access: null,
      name: "cmd",
      operation_ids: ["doThing"],
      granted: false,
      definition_changed: true,
    },
    {
      app: "22222222-2222-4222-8222-222222222222",
      folder: null,
      gateway_app: null,
      access: null,
      name: "issues",
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
    invokeOperation: vi.fn().mockResolvedValue({
      status: 200,
      content_type: "application/json",
      body_base64: "e30=",
      is_error: false,
    }),
    invokeGatewayOperation: vi.fn().mockResolvedValue({
      status: 200,
      content_type: "application/json",
      body_base64: "e30=",
      is_error: false,
    }),
    invokeFolder: vi.fn().mockResolvedValue({
      entries: [{ name: "note.txt", directory: false }],
      is_error: false,
    }),
    gatewayBaseUrl: vi.fn().mockResolvedValue("https://gateway.example.com"),
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("AppDetailView", () => {
  it("opens a granted app and drives one REST operation round trip through the bridge", async () => {
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

    const contentWindow = frame.contentWindow!;
    const posted = vi.spyOn(contentWindow, "postMessage");

    // The frame asks for a REST operation; the parent forwards it to the
    // invoke route and posts the REST result back verbatim — both
    // directions opaque passthrough.
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

  it("drives one folder operation round trip through the bridge", async () => {
    const apis = apisWith(GRANTED);
    render(<AppDetailView appId="app-1" apis={apis} onBack={() => {}} />);

    const frame = (await screen.findByTitle(
      "App: Fixture app",
    )) as HTMLIFrameElement;
    const contentWindow = frame.contentWindow!;
    const posted = vi.spyOn(contentWindow, "postMessage");

    // The frame asks for a folder listing; the parent forwards it to the
    // same invoke route and posts the folder result back verbatim.
    window.dispatchEvent(
      new MessageEvent("message", {
        data: {
          jsonrpc: "2.0",
          id: 6,
          method: "fs/list",
          params: {
            folder: "33333333-3333-4333-8333-333333333333",
            path: "reports",
          },
        },
        source: contentWindow,
      }),
    );
    await waitFor(() => expect(posted).toHaveBeenCalled());
    expect(apis.invokeFolder).toHaveBeenCalledWith(
      "app-1",
      "33333333-3333-4333-8333-333333333333",
      "list",
      "reports",
      undefined,
      undefined,
    );
    expect(posted).toHaveBeenCalledWith(
      {
        jsonrpc: "2.0",
        id: 6,
        result: {
          entries: [{ name: "note.txt", directory: false }],
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

  it("lets revoke own busy state and its grant refresh during a consent_required refusal", async () => {
    const revoke = deferred<void>();
    const apis = apisWith(GRANTED);
    apis.revoke = vi.fn().mockReturnValue(revoke.promise);
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
    fireEvent.click(screen.getByRole("button", { name: "Revoke access" }));
    expect(apis.revoke).toHaveBeenCalledWith("app-1");

    const contentWindow = frame.contentWindow!;
    const posted = vi.spyOn(contentWindow, "postMessage");
    window.dispatchEvent(
      new MessageEvent("message", {
        data: {
          jsonrpc: "2.0",
          id: 7,
          method: "operations/call",
          params: { operation_id: "listIssues" },
        },
        source: contentWindow,
      }),
    );
    await waitFor(() => expect(posted).toHaveBeenCalled());
    expect(apis.grantState).toHaveBeenCalledTimes(1);

    await act(async () => revoke.resolve(undefined));

    expect(
      await screen.findByRole("button", { name: "Allow access" }),
    ).toBeEnabled();
    expect(screen.getByRole("button", { name: "Delete app" })).toBeEnabled();
    expect(apis.grantState).toHaveBeenCalledTimes(2);
  });

  it("lets delete complete during a consent_required refusal", async () => {
    const deletion = deferred<void>();
    const onBack = vi.fn();
    const apis = apisWith(GRANTED);
    apis.deleteApp = vi.fn().mockReturnValue(deletion.promise);
    apis.invokeOperation = vi
      .fn()
      .mockRejectedValue(
        new AppInvokeRefusalError("consent_required", "Consent required"),
      );
    render(<AppDetailView appId="app-1" apis={apis} onBack={onBack} />);

    const frame = (await screen.findByTitle(
      "App: Fixture app",
    )) as HTMLIFrameElement;
    fireEvent.click(screen.getByRole("button", { name: "Delete app" }));
    const confirmation = await screen.findByRole("alertdialog");
    fireEvent.click(
      within(confirmation).getByRole("button", { name: "Delete app" }),
    );
    await waitFor(() => expect(apis.deleteApp).toHaveBeenCalledWith("app-1"));

    const contentWindow = frame.contentWindow!;
    const posted = vi.spyOn(contentWindow, "postMessage");
    window.dispatchEvent(
      new MessageEvent("message", {
        data: {
          jsonrpc: "2.0",
          id: 8,
          method: "operations/call",
          params: { operation_id: "listIssues" },
        },
        source: contentWindow,
      }),
    );
    await waitFor(() => expect(posted).toHaveBeenCalled());
    expect(apis.grantState).toHaveBeenCalledTimes(1);

    await act(async () => deletion.resolve(undefined));

    await waitFor(() => expect(onBack).toHaveBeenCalledTimes(1));
    expect(screen.getByRole("button", { name: "Revoke access" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Delete app" })).toBeEnabled();
  });

  it("gates an ungranted app behind the sheet, marks staleness, and consents body-less", async () => {
    const apis = apisWith(STALE);
    render(<AppDetailView appId="app-1" apis={apis} onBack={() => {}} />);

    // The sheet renders the server projection: the connected app's display
    // name and operation ids, and the marker for a
    // definition that changed since the previous consent.
    expect(await screen.findByText("doThing")).toBeInTheDocument();
    // The second rest_api binding renders its operation ids in the same list.
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

  it("renders the combined-consent warning when a manifest mixes a folder and operations", async () => {
    const mixed: AppGrantState = {
      granted: false,
      bindings: [
        {
          app: null,
          folder: "33333333-3333-4333-8333-333333333333",
          gateway_app: null,
          access: "read_write",
          name: "Tax documents",
          operation_ids: null,
          granted: false,
          definition_changed: false,
        },
        {
          app: "22222222-2222-4222-8222-222222222222",
          folder: null,
          gateway_app: null,
          access: null,
          name: "issues",
          operation_ids: ["listIssues"],
          granted: false,
          definition_changed: false,
        },
      ],
    };
    render(
      <AppDetailView appId="app-1" apis={apisWith(mixed)} onBack={() => {}} />,
    );

    // The folder row names the folder, its read line, and its louder write
    // line; the exfiltration warning names both sides in prose.
    expect(await screen.findByText("Tax documents")).toBeInTheDocument();
    expect(screen.getByText("Read files and folders")).toBeInTheDocument();
    expect(
      screen.getByText("Create and replace files in this folder"),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "This app can read 'Tax documents' and send data to 'issues'.",
      ),
    ).toBeInTheDocument();
  });

  it("ignores an earlier app load that resolves after navigation", async () => {
    const oldDetail = deferred<AppDetail>();
    const oldGrant = deferred<AppGrantState>();
    const currentDetail: AppDetail = {
      ...DETAIL,
      id: "app-2",
      name: "Current app",
    };
    const apis = apisWith(GRANTED);
    apis.get = vi
      .fn()
      .mockReturnValueOnce(oldDetail.promise)
      .mockResolvedValueOnce(currentDetail);
    apis.grantState = vi
      .fn()
      .mockReturnValueOnce(oldGrant.promise)
      .mockResolvedValueOnce(GRANTED);
    const { rerender } = render(
      <AppDetailView appId="app-1" apis={apis} onBack={() => {}} />,
    );

    rerender(<AppDetailView appId="app-2" apis={apis} onBack={() => {}} />);
    expect(await screen.findByTitle("App: Current app")).toBeInTheDocument();

    await act(async () => {
      oldDetail.resolve(DETAIL);
      oldGrant.resolve(GRANTED);
    });
    expect(screen.getByTitle("App: Current app")).toBeInTheDocument();
    expect(screen.queryByTitle("App: Fixture app")).not.toBeInTheDocument();
  });

  it("counts a gateway row as network access and marks who executes it", async () => {
    const mixed: AppGrantState = {
      granted: false,
      bindings: [
        {
          app: null,
          folder: "33333333-3333-4333-8333-333333333333",
          gateway_app: null,
          access: "read",
          name: "Tax documents",
          operation_ids: null,
          granted: false,
          definition_changed: false,
        },
        {
          app: null,
          folder: null,
          gateway_app: "gw-issues",
          access: null,
          name: "Issues (gateway)",
          operation_ids: ["listIssues"],
          granted: false,
          definition_changed: false,
        },
      ],
    };
    render(
      <AppDetailView appId="app-1" apis={apisWith(mixed)} onBack={() => {}} />,
    );

    // A gateway app is network access exactly as a local one is, so the
    // combined-consent warning fires on a folder row plus a gateway row.
    expect(
      await screen.findByText(
        "This app can read 'Tax documents' and send data to 'Issues (gateway)'.",
      ),
    ).toBeInTheDocument();
    // The row still says who runs the call: the org's gateway, not this
    // machine holding a credential for it.
    expect(
      screen.getByText("Runs through your organization’s gateway, as you"),
    ).toBeInTheDocument();
    expect(screen.getByText("listIssues")).toBeInTheDocument();
  });

  it("offers a connect affordance when the gateway refuses a relay for want of the viewer's credential", async () => {
    const apis = apisWith(GRANTED);
    apis.invokeGatewayOperation = vi
      .fn()
      .mockRejectedValue(
        new AppInvokeRefusalError(
          "gateway_authorization_required",
          "connect Issues (gateway) at your model gateway to continue: sign in",
        ),
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
          id: 9,
          method: "operations/call",
          params: {
            connected_app_id: "gw-issues",
            operation_id: "listIssues",
            query: { state: "open" },
          },
        },
        source: contentWindow,
      }),
    );

    // The frame's own call is routed to the gateway leg and refused
    // machine-readably; the host answers with the affordance only the viewer
    // can act on, and the frame stays mounted — nothing local was revoked.
    await waitFor(() =>
      expect(posted).toHaveBeenCalledWith(
        expect.objectContaining({
          id: 9,
          error: expect.objectContaining({
            data: { kind: "gateway_authorization_required" },
          }),
        }),
        "*",
      ),
    );
    expect(apis.invokeGatewayOperation).toHaveBeenCalledWith(
      "app-1",
      "gw-issues",
      "listIssues",
      undefined,
      { state: "open" },
      undefined,
    );
    const connect = await screen.findByRole("button", {
      name: "Connect at gateway",
    });
    expect(
      screen.getByText(/connect Issues \(gateway\) at your model gateway/),
    ).toBeInTheDocument();
    expect(screen.getByTitle("App: Fixture app")).toBeInTheDocument();

    // The button sends the viewer to the gateway's own origin — the gateway's
    // SSO is the handoff — and the banner clears once it has.
    fireEvent.click(connect);
    await waitFor(() => expect(apis.gatewayBaseUrl).toHaveBeenCalled());
    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "Connect at gateway" }),
      ).not.toBeInTheDocument(),
    );
  });
});
