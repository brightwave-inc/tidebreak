import type { Meta, StoryObj } from "@storybook/react-vite";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { PDFDocument, StandardFonts, rgb } from "pdf-lib";
import { expect, userEvent, waitFor, within } from "storybook/test";

import {
  ApiClient,
  HttpError,
  type Chat,
  type DocumentDetail,
  type Project,
} from "@/api";
import { AppContextProvider, type AppContextValue } from "@/AppContext";
import { useChatListStore } from "@/ChatListStore";
import { useChatSessionStore } from "@/ChatSessionStore";
import { clearFileDownloadCache } from "@/document/useFileDownload";
import { DocumentDetailRoot } from "@/document-detail/DocumentDetailRoot";
import {
  layoutFromSearch,
  panelSearchFrom,
  type PanelSearch,
} from "@/panel/panelUrl";
import { useProjectListStore } from "@/ProjectListStore";

const CHAT_ID = "chat-storybook";
const DOCUMENT_ID = "document-renewals";
const PROJECT_ID = "project-renewals";
const CITATION_ID = "citation-renewal-risk";

const renewalMemo = `Renewal operating memo

Summary
The next 90 days contain 62% of forecast renewal value. Five enterprise accounts represent $1.8M in annual recurring revenue and need named executive sponsors.

Account priorities
Northstar Health — Adoption is strong, but legal review has not started.
Fieldline Logistics — Pricing approval expires on September 12.
Juniper Public Media — Security review is complete; procurement owns the next step.
Harbor Energy — Product usage fell 14% after the July rollout.
Canopy Systems — Expansion depends on the analytics migration plan.

Recommended actions
Assign an executive sponsor to each priority account.
Confirm the procurement timeline before September 5.
Review adoption risk with customer success every Friday.`;

type Scenario =
  | "success"
  | "loading"
  | "retriable"
  | "not-found"
  | "download-rejected"
  | "sharing-success"
  | "sharing-deferred"
  | "sharing-rejected"
  | "citation"
  | "original-success"
  | "original-loading"
  | "original-failed";

function documentInfo(scenario: Scenario): DocumentDetail {
  return {
    document_id: DOCUMENT_ID,
    media_type: "application/pdf",
    title: "Q3 renewal operating memo.pdf",
    readable: true,
    has_original_bytes:
      scenario === "original-success" ||
      scenario === "original-loading" ||
      scenario === "original-failed",
    updated_at: "2026-08-21T15:30:00.000Z",
    content: renewalMemo,
  };
}

function pending<T>(): Promise<T> {
  return new Promise(() => {});
}

async function originalPdfBytes(): Promise<Uint8Array> {
  const document = await PDFDocument.create();
  const page = document.addPage([612, 792]);
  const font = await document.embedFont(StandardFonts.Helvetica);
  page.drawText("Q3 renewal operating memo", {
    x: 72,
    y: 700,
    size: 22,
    font,
    color: rgb(0.13, 0.16, 0.14),
  });
  page.drawText("Five priority accounts need named executive sponsors.", {
    x: 72,
    y: 660,
    size: 12,
    font,
    color: rgb(0.3, 0.35, 0.31),
  });
  return document.save();
}

function storyClient(scenario: Scenario): ApiClient {
  const info = documentInfo(scenario);
  const client = new ApiClient("https://storybook.invalid", "storybook");
  let documentAttempts = 0;

  client.getChatDocument = async () => {
    if (scenario === "loading") return pending<DocumentDetail>();
    if (scenario === "not-found") {
      throw new HttpError(404, "404: document not found");
    }
    if (scenario === "retriable" && documentAttempts++ === 0) {
      throw new HttpError(503, "503: service unavailable");
    }
    return info;
  };
  client.getChatDocumentFile = async () => {
    if (scenario === "original-loading") {
      return pending<{ bytes: Uint8Array; contentType: string | null }>();
    }
    if (scenario === "original-failed") {
      throw new Error("Original file download failed.");
    }
    return {
      bytes:
        scenario === "original-success"
          ? await originalPdfBytes()
          : new TextEncoder().encode(info.content),
      contentType: info.media_type,
    };
  };
  client.promoteDocumentToProject = async () => {
    if (scenario === "sharing-deferred") {
      return pending<{ document_id: string }>();
    }
    if (scenario === "sharing-rejected") {
      throw new Error("The file could not be added to the project.");
    }
    return { document_id: "project-document-renewals" };
  };

  return client;
}

function storyDownload(scenario: Scenario) {
  let attempts = 0;
  return async () => {
    if (scenario === "download-rejected" && attempts++ === 0) {
      throw new Error("Could not save that source.");
    }
    return true;
  };
}

function appContext(client: ApiClient): AppContextValue {
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

function resetStoryStores() {
  useChatListStore.setState({
    chats: [],
    chatsLoaded: false,
    chatsError: null,
    creatingChat: false,
    deletingChatId: null,
    renamingChatId: null,
    renameChatDraft: "",
    savingTitle: false,
    derivedTitleChatId: null,
    streamedTitles: {},
  });
  useProjectListStore.setState({
    projects: [],
    projectsLoaded: false,
    creatingProject: false,
    deletingProjectId: null,
    renamingProjectId: null,
    renameProjectDraft: "",
    savingProjectTitle: false,
    expandedProjectIds: [],
  });
  useChatSessionStore.getState().reset();
  clearFileDownloadCache();
}

function seedStoryStores(scenario: Scenario) {
  resetStoryStores();

  const projectId = scenario.startsWith("sharing-") ? PROJECT_ID : null;
  const chat: Chat = {
    id: CHAT_ID,
    project_id: projectId,
    title: "Renewal planning",
    model: null,
    reasoning_effort: null,
    permission_mode: "ask",
    network_policy: { mode: "off" },
    attachment_revision: 0,
    memory_incognito: false,
    root_attachments: [],
    created_at: "2026-08-21T15:30:00.000Z",
  };
  useChatListStore.getState().setChats([chat]);

  const projects: Project[] = projectId
    ? [
        {
          id: PROJECT_ID,
          title: "Renewal operations",
          attachment_revision: 0,
          root_attachments: [],
          created_at: "2026-08-20T13:00:00.000Z",
        },
      ]
    : [];
  useProjectListStore.getState().setProjects(projects);

  if (scenario === "citation") {
    useChatSessionStore.getState().update((session) => ({
      ...session,
      messages: [
        {
          id: "message-renewal-summary",
          role: "assistant",
          text: "Two accounts need immediate commercial follow-up.",
          sources: [
            {
              id: CITATION_ID,
              ordinal: 1,
              documentId: DOCUMENT_ID,
              locator: { kind: "lines", start: 6, end: 8 },
            },
          ],
          createdAt: "2026-08-21T15:34:00.000Z",
        },
      ],
    }));
  }
}

function storyUrl(scenario: Scenario): string {
  const citation = scenario === "citation" ? `.${CITATION_ID}` : "";
  return `/c/${CHAT_ID}?tabs=document.${DOCUMENT_ID}${citation}`;
}

function storyRouter(
  initialUrl: string,
  download: (chatId: string, documentId: string) => Promise<boolean>,
) {
  const rootRoute = createRootRoute();
  const chatRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/c/$chatId",
    validateSearch: (search: Record<string, unknown>): PanelSearch =>
      panelSearchFrom(search),
    component: function DocumentRoute() {
      const { chatId } = chatRoute.useParams();
      const layout = layoutFromSearch(chatRoute.useSearch());
      const panel = layout.tabs[layout.activeIndex];
      if (panel?.type !== "document") return null;

      return (
        <DocumentDetailRoot
          chatId={chatId}
          documentID={panel.documentId}
          citationId={panel.citationId}
          download={download}
          canDownload
        />
      );
    },
  });

  return createRouter({
    routeTree: rootRoute.addChildren([chatRoute]),
    history: createMemoryHistory({ initialEntries: [initialUrl] }),
  });
}

function DocumentDetailRootStory({ scenario }: { scenario: Scenario }) {
  const [state] = useState(() => {
    seedStoryStores(scenario);
    const client = storyClient(scenario);
    const download = storyDownload(scenario);
    return {
      client,
      router: storyRouter(storyUrl(scenario), download),
    };
  });

  useEffect(() => () => resetStoryStores(), []);

  return (
    <AppContextProvider value={appContext(state.client)}>
      <div className="app-shell h-screen min-h-0 w-full overflow-hidden bg-page-background text-foreground">
        <RouterProvider router={state.router as never} />
      </div>
    </AppContextProvider>
  );
}

const meta = {
  title: "Documents/Detail",
  component: DocumentDetailRootStory,
  parameters: { layout: "fullscreen" },
  args: { scenario: "success" },
  render: (args) => <DocumentDetailRootStory key={args.scenario} {...args} />,
} satisfies Meta<typeof DocumentDetailRootStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Success: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByText(/62% of forecast renewal value/),
    ).toBeVisible();
  },
};

export const Loading: Story = {
  args: { scenario: "loading" },
};

export const RetriableFailure: Story = {
  args: { scenario: "retriable" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByText("The document could not be loaded (503)."),
    ).toBeVisible();
    await expect(
      canvas.getByRole("button", { name: "Try again" }),
    ).toBeVisible();
    await userEvent.click(canvas.getByRole("button", { name: "Try again" }));
    await expect(
      await canvas.findByText(/62% of forecast renewal value/),
    ).toBeVisible();
  },
};

export const TerminalNotFound: Story = {
  args: { scenario: "not-found" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByText("The document is no longer available."),
    ).toBeVisible();
    await expect(
      canvas.queryByRole("button", { name: "Try again" }),
    ).not.toBeInTheDocument();
  },
};

export const DownloadFailureAndRetry: Story = {
  args: { scenario: "download-rejected" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await canvas.findByText(/62% of forecast renewal value/);
    const download = canvas.getByRole("button", { name: "Download" });

    await userEvent.click(download);
    await expect(await canvas.findByRole("alert")).toHaveTextContent(
      "Could not save that source.",
    );
    await expect(download).toBeEnabled();

    await userEvent.click(download);
    await waitFor(() => {
      expect(canvas.queryByRole("alert")).not.toBeInTheDocument();
    });
    await expect(download).toBeEnabled();
  },
};

export const ProjectSharing: Story = {
  args: { scenario: "sharing-success" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Add to project" }),
    );
    await expect(
      await canvas.findByRole("button", { name: "In the project" }),
    ).toBeDisabled();
  },
};

export const ProjectSharingPending: Story = {
  args: { scenario: "sharing-deferred" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const share = await canvas.findByRole("button", {
      name: "Add to project",
    });
    await expect(share).toBeEnabled();
    await userEvent.click(share);
    await expect(share).toBeDisabled();
  },
};

export const ProjectSharingFailure: Story = {
  args: { scenario: "sharing-rejected" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const share = await canvas.findByRole("button", {
      name: "Add to project",
    });
    await userEvent.click(share);
    await waitFor(() => expect(share).toBeEnabled());
    await expect(share).toHaveAccessibleName("Add to project");
  },
};

export const CitationHighlight: Story = {
  args: { scenario: "citation" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByLabelText("Cited passage"),
    ).toHaveTextContent("Account priorities");
  },
};

export const OriginalFileLoading: Story = {
  args: { scenario: "original-loading" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(await canvas.findByText("Loading document…")).toBeVisible();
  },
};

export const OriginalFileSuccess: Story = {
  args: { scenario: "original-success" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByRole("button", { name: "Rotate clockwise" }),
    ).toBeVisible();
  },
};

export const OriginalFileFailure: Story = {
  args: { scenario: "original-failed" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByText("This document could not be loaded."),
    ).toBeVisible();
  },
};

export const Compact: Story = {
  globals: { viewport: { value: "compact", isRotated: false } },
};
