// @vitest-environment jsdom
import { cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { HttpError, type ApiClient, type ProjectDocument } from "./api";
import { AppContextProvider, type AppContextValue } from "./AppContext";
import { ProjectFilesView } from "./ProjectFilesView";
import { useProjectListStore } from "./ProjectListStore";
import { renderWithRouter } from "./test/router";

const PROJECT_ID = "project-1";

const brief: ProjectDocument = {
  document_id: "document-1",
  title: "Q3 renewal plan.md",
  media_type: "text/markdown",
  source_byte_len: 2_048,
} as ProjectDocument;

function renderProject(client: Partial<ApiClient>) {
  useProjectListStore.setState({
    projects: [
      {
        id: PROJECT_ID,
        title: "Desktop release",
        attachment_revision: 1,
        root_attachments: [],
        instructions: "",
        created_at: "2026-08-20T10:00:00.000Z",
      },
    ],
    projectsLoaded: true,
  });
  return renderWithRouter(
    <AppContextProvider value={{ client } as AppContextValue}>
      <ProjectFilesView projectId={PROJECT_ID} />
    </AppContextProvider>,
    { initialUrl: "/" },
  );
}

afterEach(() => {
  cleanup();
  useProjectListStore.setState({ projects: [], projectsLoaded: false });
});

describe("ProjectFilesView", () => {
  it("says the files did not load in words, and loads them again on Try again", async () => {
    const listProjectDocuments = vi
      .fn()
      .mockRejectedValueOnce(new TypeError("Load failed"))
      .mockResolvedValue({ documents: [brief], next_cursor: null });
    await renderProject({ listProjectDocuments });

    const failure = await screen.findByRole("alert");
    expect(failure).toHaveTextContent("Could not load the project's files");
    expect(failure).toHaveTextContent("Tidebreak could not reach its server.");
    expect(failure).not.toHaveTextContent(/TypeError|Load failed/);

    await userEvent.click(
      within(failure).getByRole("button", { name: "Try again" }),
    );

    expect(await screen.findByText("Q3 renewal plan.md")).toBeInTheDocument();
    expect(listProjectDocuments).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("keeps the list when a removal fails, and offers no reload for it", async () => {
    const deleteProjectDocument = vi.fn().mockRejectedValue(
      new HttpError(409, "409: the file is still attached", "conflict", {
        kind: "conflict",
        message: "the file is still attached",
      }),
    );
    await renderProject({
      listProjectDocuments: vi
        .fn()
        .mockResolvedValue({ documents: [brief], next_cursor: null }),
      deleteProjectDocument,
    });

    await userEvent.click(
      await screen.findByRole("button", {
        name: "Remove Q3 renewal plan.md from the project",
      }),
    );
    await userEvent.click(
      await screen.findByRole("button", { name: "Remove" }),
    );

    const failure = await screen.findByRole("alert");
    expect(failure).toHaveTextContent("The file is still attached");
    expect(failure).not.toHaveTextContent("409");
    expect(within(failure).queryByRole("button")).toBeNull();
    await waitFor(() =>
      expect(screen.getByText("Q3 renewal plan.md")).toBeInTheDocument(),
    );
  });
});
