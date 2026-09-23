// @vitest-environment jsdom
/**
 * The first message in a new workspace, end to end on the desktop side of
 * the server boundary.
 *
 * It drives the real start surface, composer, session create, image
 * publication, and API client, with the network stubbed at `fetch` and
 * `XMLHttpRequest`. The draft is typed, a long paste becomes pasted text, and
 * an image is pasted; the composer then remounts, as a Settings round trip
 * does, before the send. The requests that leave the renderer must match
 * `tidebreak-server-api/fixtures/first-message.json`, which the server's
 * `tests::code_first_message` replays against a real server to check that
 * all three reach the first turn.
 */
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";

import {
  act,
  cleanup,
  createEvent,
  fireEvent,
  screen,
  waitFor,
} from "@testing-library/react";
import { useMemo } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import { ApiClient } from "../api/client";
import { useComposerDrafts } from "../ComposerDrafts";
import { codeSession, codeWorkspace, harnessDoctor } from "../stories/fixtures";
import { renderWithRouter } from "../test/router";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { CodeComposer } from "./CodeComposer";
import { useCodeComposerStatus } from "./CodeSessionSend";
import { resetCodeSessionRegistry } from "./CodeSessionRegistry";
import { useCodeUiStore } from "./CodeUiStore";
import { StartSessionPrompt } from "./StartSessionPrompt";
import { useWorkspaceSessions } from "./workspace/useWorkspaceSessions";

type RecordedRequest = {
  method: string;
  path: string;
  content_type?: string;
  json?: unknown;
  body?: "image";
};

type FirstMessageFixture = {
  about: string[];
  draft: string;
  pasted_text: string;
  image: { file_name: string; media_type: string; base64: string };
  requests: RecordedRequest[];
};

// Tests run from the UI package; the fixture sits with the server's.
const FIXTURE_PATH = path.resolve(
  process.cwd(),
  "../../tidebreak-server-api/fixtures/first-message.json",
);
const fixture: FirstMessageFixture = JSON.parse(
  readFileSync(FIXTURE_PATH, "utf8"),
);

const BASE_URL = "http://tidebreak.test";
const WORKSPACE = "ws-first-message";
const SESSION = "sess-first-message";
const REPO = "repo-first-message";
const IMAGE_ID = "6b0c7a52-1f3e-4b8f-9a55-2d4e0c1f7a90";

const recorded: RecordedRequest[] = [];
let uploadedFile: Blob | null = null;

/** The ids this test mints, as the fixture names them. */
function normalize(value: string): string {
  return value
    .replaceAll(WORKSPACE, "{workspace}")
    .replaceAll(SESSION, "{session}")
    .replaceAll(IMAGE_ID, "{image}");
}

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

/** The server this test stands in for, answering the way the real routes do. */
async function serve(
  input: RequestInfo | URL,
  init?: RequestInit,
): Promise<Response> {
  const url = new URL(String(input));
  const method = (init?.method ?? "GET").toUpperCase();
  const path = url.pathname;
  if (method !== "GET") {
    const parsed = init?.body ? JSON.parse(String(init.body)) : undefined;
    recorded.push({
      method,
      path: normalize(path),
      json: JSON.parse(normalize(JSON.stringify(parsed))),
    });
  }
  if (method === "GET" && path === `/code/workspaces/${WORKSPACE}`) {
    return json(200, {
      ...codeWorkspace,
      id: WORKSPACE,
      repo_id: REPO,
      is_owner: true,
    });
  }
  if (method === "GET" && path === `/code/workspaces/${WORKSPACE}/sessions`) {
    return json(200, []);
  }
  if (method === "GET" && path === `/code/repos/${REPO}`) {
    return json(200, {
      id: REPO,
      root_path: "/Users/sam/src/auth",
      display_name: "auth",
      default_base_ref: "main",
      branch_prefix: "sam",
      quick_actions: [],
      created_at: "2026-09-20T10:00:00.000Z",
    });
  }
  if (method === "GET" && path === "/code/harnesses/claude_code/models") {
    return json(200, { kind: "claude_code", models: [] });
  }
  if (method === "POST" && path === `/code/workspaces/${WORKSPACE}/sessions`) {
    const body = JSON.parse(String(init?.body));
    return json(201, {
      ...codeSession,
      id: SESSION,
      workspace_id: WORKSPACE,
      permission_mode: body.permission_mode,
      lifecycle: "idle",
      attention: { state: { type: "idle" }, source: "lifecycle" },
    });
  }
  if (method === "POST" && path === `/sessions/${SESSION}/turns`) {
    const body = JSON.parse(String(init?.body));
    // Answered on acceptance: the turn is still running.
    return json(202, {
      id: "turn-first-message",
      session_id: SESSION,
      ordinal: 1,
      status: "running",
      fast_mode: false,
      user_input: body.message,
      attachments: [],
      started_at: "2026-09-23T12:00:00.000Z",
    });
  }
  return json(404, { error: `no route for ${method} ${path}` });
}

/** The image upload's transport, which is an XHR for its progress events. */
class RecordingXhr {
  status = 0;
  responseText = "";
  upload: { onprogress: ((event: ProgressEvent) => void) | null } = {
    onprogress: null,
  };
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onabort: (() => void) | null = null;
  private method = "";
  private url = "";
  private contentType = "";

  open(method: string, url: string) {
    this.method = method;
    this.url = url;
  }

  setRequestHeader(name: string, value: string) {
    if (name.toLowerCase() === "content-type") this.contentType = value;
  }

  send(body: Blob) {
    uploadedFile = body;
    recorded.push({
      method: this.method,
      path: normalize(new URL(this.url).pathname),
      content_type: this.contentType,
      body: "image",
    });
    queueMicrotask(() => {
      this.status = 201;
      this.responseText = JSON.stringify({
        attachment_id: IMAGE_ID,
        media_type: "image/png",
        width: 1,
        height: 1,
        byte_len: body.size,
      });
      this.onload?.();
    });
  }

  abort() {
    this.onabort?.();
  }
}

function app(client: ApiClient): AppContextValue {
  return {
    client,
    models: [],
    defaultModelKey: null,
    providers: [],
    refreshCatalog: async () => {},
    refreshChats: async () => {},
    status: "",
    setStatus: () => {},
    newChat: () => {},
    deleteChat: () => {},
    startRename: () => {},
    commitRename: () => {},
    cancelRename: () => {},
    newProject: async () => false,
    deleteProject: () => {},
    startProjectRename: () => {},
    commitProjectRename: () => {},
    cancelProjectRename: () => {},
    newChatInProject: () => {},
    moveChatToProject: () => {},
    updateState: { status: "idle", version: null, error: null, enabled: false },
    updateUpToDate: false,
    checkForUpdate: async () => ({
      status: "idle",
      version: null,
      error: null,
      enabled: false,
    }),
    attachment: "local",
    restartForUpdate: async () => {},
  };
}

/**
 * The workspace page's first-agent wiring, without the page's chrome: the
 * start surface until a session exists, then that session's composer.
 */
function FirstMessageSurface({ client }: { client: ApiClient }) {
  const navigate = useMemo(() => vi.fn(), []);
  const workspace = useWorkspaceSessions({
    workspaceId: WORKSPACE,
    client,
    models: [],
    defaultModelKey: null,
    taskParam: undefined,
    navigate: navigate as never,
    focusConversationPane: () => {},
  });
  if (workspace.session) {
    return (
      <CodeComposer
        sessionId={workspace.session.id}
        promptScope={WORKSPACE}
        running={false}
        permissionMode={workspace.session.permission_mode}
        onInterrupt={() => {}}
      />
    );
  }
  if (!workspace.workspace) return null;
  return (
    <StartSessionPrompt
      workspaceId={WORKSPACE}
      harnesses={[harnessDoctor.harnesses[0]]}
      starting={workspace.starting}
      selectedMode={null}
      onSelectMode={() => {}}
      onStart={workspace.startSession}
      client={client}
    />
  );
}

async function renderSurface(client: ApiClient) {
  await renderWithRouter(
    <AppContextProvider value={app(client)}>
      <FirstMessageSurface client={client} />
    </AppContextProvider>,
    { initialUrl: "/" },
  );
  return screen.findByRole("textbox", { name: "Message" });
}

/** Write what the desktop sent as the fixture's requests, and answer them. */
function record(requests: RecordedRequest[]): RecordedRequest[] {
  writeFileSync(
    FIXTURE_PATH,
    `${JSON.stringify({ ...fixture, requests }, null, 2)}\n`,
  );
  return requests;
}

function imageFile(): File {
  const bytes = Uint8Array.from(atob(fixture.image.base64), (char) =>
    char.charCodeAt(0),
  );
  return new File([bytes], fixture.image.file_name, {
    type: fixture.image.media_type,
  });
}

beforeEach(() => {
  recorded.length = 0;
  uploadedFile = null;
  vi.stubGlobal("fetch", vi.fn(serve));
  vi.stubGlobal("XMLHttpRequest", RecordingXhr);
  URL.createObjectURL = vi.fn((): string => "blob:first-message");
  URL.revokeObjectURL = vi.fn();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  resetCodeSessionRegistry();
  useCodeCatalogStore.getState().reset();
  useCodeUiStore.setState({ pendingComposerPrompt: null, lastCreate: null });
  useComposerDrafts.setState({ drafts: {}, attachments: {} });
  useCodeComposerStatus.setState({ byKey: {} });
  window.sessionStorage.clear();
});

it("sends a restored draft, pasted text, and an image as a new workspace's first turn", async () => {
  const client = new ApiClient(BASE_URL, "token");
  let box = await renderSurface(client);

  fireEvent.change(box, { target: { value: fixture.draft } });
  const paste = createEvent.paste(box, {
    clipboardData: {
      files: [],
      getData: (type: string) =>
        type === "text/plain" ? fixture.pasted_text : "",
    },
  });
  fireEvent(box, paste);
  expect(paste.defaultPrevented).toBe(true);
  const image = imageFile();
  fireEvent(box, createEvent.paste(box, { clipboardData: { files: [image] } }));
  expect(await screen.findByText("Pasted text")).toBeInTheDocument();
  expect(screen.getByLabelText("Attached images")).toBeInTheDocument();

  // Settings replaces the page and brings it back. Nothing is sent yet, and
  // everything the reader put in the composer is still there.
  cleanup();
  box = await renderSurface(client);
  expect(box).toHaveValue(fixture.draft);
  expect(screen.getByText("Pasted text")).toBeInTheDocument();
  expect(screen.getByLabelText("Attached images")).toBeInTheDocument();
  expect(recorded).toEqual([]);

  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));
  });
  await waitFor(() => expect(recorded).toHaveLength(3));

  const expected =
    process.env.TIDEBREAK_RECORD_FIRST_MESSAGE === "1"
      ? record(recorded)
      : fixture.requests;
  // The session is created first; the image is published to it; one turn
  // carries the draft, the paste, and the image.
  expect(recorded).toEqual(expected);
  expect(uploadedFile).toBe(image);

  // The session's composer took over and is empty once the turn is accepted.
  await waitFor(() =>
    expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue(""),
  );
  expect(screen.queryByText("Pasted text")).toBeNull();
  expect(screen.queryByLabelText("Attached images")).toBeNull();
  expect(screen.queryByRole("alert")).toBeNull();
});
