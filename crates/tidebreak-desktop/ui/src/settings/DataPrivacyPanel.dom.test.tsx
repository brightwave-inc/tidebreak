// @vitest-environment jsdom
import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { HttpError, type DataOverview } from "@/api";
import { DELETE_ALL_DATA_PHRASE } from "@/host";
import { STORAGE_KEY as THEME_KEY } from "@/theme";
import {
  DataPrivacyPanel,
  type DataPrivacyHost,
  type ExportableConversation,
} from "./DataPrivacyPanel";
import { OUTBOUND_TRAFFIC } from "./dataPrivacy";

const overview: DataOverview = {
  data_dir: "/Users/alex/Library/Application Support/io.brightwave.tidebreak",
  storage: "sqlite",
  usage: [
    { category: "database", bytes: 12_582_912 },
    { category: "attachments", bytes: 3_145_728 },
    { category: "outputs", bytes: 524_288 },
    { category: "logs", bytes: 65_536 },
    { category: "engine_tools", bytes: 419_430_400 },
    { category: "backups", bytes: 25_165_824 },
    { category: "other", bytes: 4_096 },
  ],
  total_bytes: 460_918_784,
};

const conversations: ExportableConversation[] = [
  {
    id: "9d5d84a0-6ba6-4c73-9e10-000000000001",
    title: "Launch plan",
    created_at: "2026-09-20T10:00:00Z",
  },
  {
    id: "9d5d84a0-6ba6-4c73-9e10-000000000002",
    title: "Quarterly numbers",
    created_at: "2026-09-21T10:00:00Z",
  },
];

function renderPanel({
  data = overview,
  overviewError,
  attachedRemotely = false,
  local = true,
}: {
  data?: DataOverview;
  overviewError?: Error;
  attachedRemotely?: boolean;
  local?: boolean;
} = {}) {
  const client = {
    getDataOverview: vi.fn(async () => {
      if (overviewError) throw overviewError;
      return data;
    }),
    resetSettings: vi.fn(async () => ({}) as never),
    downloadProfileBackup: vi.fn(),
    downloadConversationExport: vi.fn(),
  };
  const host: DataPrivacyHost = {
    local,
    reveal: vi.fn(async () => undefined),
    saveBackup: vi.fn(async () => ({
      path: "/Users/alex/Desktop/backup.tar.gz",
      bytes: 1_024,
      count: 12,
    })),
    saveExport: vi.fn(async () => ({
      path: "/Users/alex/Desktop/chats.json",
      bytes: 2_048,
      count: 1,
    })),
    deleteAllData: vi.fn(async () => undefined),
  };
  const onReload = vi.fn();
  const onOpenSection = vi.fn();
  render(
    <DataPrivacyPanel
      client={client as never}
      host={host}
      attachedRemotely={attachedRemotely}
      conversations={conversations}
      onOpenSection={onOpenSection}
      onReload={onReload}
      userAgent="Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)"
    />,
  );
  return { client, host, onReload, onOpenSection };
}

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

describe("DataPrivacyPanel", () => {
  it("names the data folder, its disk use, and everything that leaves this computer", async () => {
    const { host, onOpenSection } = renderPanel();
    expect(await screen.findByText(overview.data_dir)).toBeInTheDocument();

    const usage = within(
      screen.getByRole("list", { name: "Disk use by category" }),
    );
    expect(usage.getAllByRole("listitem")).toHaveLength(7);
    expect(usage.getByText("Engine tools")).toBeInTheDocument();
    expect(usage.getByText("400 MB")).toBeInTheDocument();

    const outbound = within(
      screen.getByRole("list", { name: "What leaves this computer" }),
    );
    expect(outbound.getAllByRole("listitem")).toHaveLength(
      OUTBOUND_TRAFFIC.length,
    );
    await userEvent.click(
      outbound.getByRole("button", { name: "Open Updates" }),
    );
    expect(onOpenSection).toHaveBeenCalledWith("updates");

    await userEvent.click(
      screen.getByRole("button", { name: "Reveal in Finder" }),
    );
    expect(host.reveal).toHaveBeenCalledOnce();
  });

  it("deletes all data only after the phrase is typed, and clears this window's storage", async () => {
    window.localStorage.setItem(THEME_KEY, "dark");
    const { host } = renderPanel();
    await userEvent.click(
      await screen.findByRole("button", { name: "Delete all data…" }),
    );
    const dialog = await screen.findByRole("alertdialog");
    const confirm = within(dialog).getByRole("button", {
      name: "Delete all data",
    });
    expect(confirm).toBeDisabled();
    expect(within(dialog).getByText(/removes your keys/)).toBeVisible();

    await userEvent.type(
      within(dialog).getByLabelText(
        `Type ${DELETE_ALL_DATA_PHRASE} to confirm.`,
      ),
      DELETE_ALL_DATA_PHRASE,
    );
    await userEvent.click(confirm);

    await waitFor(() =>
      expect(host.deleteAllData).toHaveBeenCalledWith(DELETE_ALL_DATA_PHRASE),
    );
    expect(window.localStorage.getItem(THEME_KEY)).toBeNull();
  });

  it("resets settings after saying exactly what resets, then reloads", async () => {
    window.localStorage.setItem(THEME_KEY, "dark");
    window.localStorage.setItem("tidebreak.command-palette-recents", "[]");
    const { client, onReload } = renderPanel();
    await userEvent.click(
      await screen.findByRole("button", { name: "Reset settings…" }),
    );
    const dialog = await screen.findByRole("alertdialog");
    expect(within(dialog).getByText(/Git branch naming/)).toBeVisible();
    expect(
      within(dialog).getByText(/Conversations, memory, instructions, keys/),
    ).toBeVisible();
    expect(client.resetSettings).not.toHaveBeenCalled();

    await userEvent.click(
      within(dialog).getByRole("button", { name: "Reset settings" }),
    );
    await waitFor(() => expect(client.resetSettings).toHaveBeenCalledOnce());
    await waitFor(() => expect(onReload).toHaveBeenCalledOnce());
    expect(window.localStorage.getItem(THEME_KEY)).toBeNull();
    // Only preferences go: other state this window keeps stays.
    expect(
      window.localStorage.getItem("tidebreak.command-palette-recents"),
    ).toBe("[]");
  });

  it("exports the conversations you choose in the format you choose", async () => {
    const { host } = renderPanel();
    await userEvent.click(
      await screen.findByRole("button", { name: "Export…" }),
    );
    const dialog = await screen.findByRole("dialog");
    await userEvent.click(within(dialog).getByRole("radio", { name: "JSON" }));
    await userEvent.click(
      within(dialog).getByRole("radio", { name: "Choose" }),
    );
    const exportButton = within(dialog).getByRole("button", {
      name: "Export",
    });
    expect(exportButton).toBeDisabled();
    await userEvent.click(
      within(dialog).getByRole("checkbox", { name: "Launch plan" }),
    );
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Export 1" }),
    );

    await waitFor(() =>
      expect(host.saveExport).toHaveBeenCalledWith({
        format: "json",
        chat_ids: [conversations[0].id],
      }),
    );
  });

  it("backs up through the save dialog, and says why when it cannot", async () => {
    const { host } = renderPanel();
    await userEvent.click(
      await screen.findByRole("button", { name: "Back up…" }),
    );
    await waitFor(() => expect(host.saveBackup).toHaveBeenCalledOnce());
    cleanup();

    renderPanel({
      data: {
        ...overview,
        storage: "postgres",
        backup_unavailable:
          "This server keeps conversations in PostgreSQL. Back it up with pg_dump.",
      },
    });
    expect(
      await screen.findByText(/Back it up with pg_dump/),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Back up…" })).toBeDisabled();
  });

  it("offers nothing that acts on this computer while attached to another machine", async () => {
    renderPanel({ attachedRemotely: true });
    expect(
      await screen.findByText("This window works on another machine"),
    ).toBeInTheDocument();
    expect(screen.getByText(overview.data_dir)).toBeInTheDocument();
    for (const name of [
      "Reveal in Finder",
      "Back up…",
      "Export…",
      "Reset settings…",
      "Delete all data…",
    ]) {
      expect(screen.queryByRole("button", { name })).not.toBeInTheDocument();
    }
  });

  it("leaves a member of a shared server the export and nothing of the administrator's", async () => {
    renderPanel({
      local: false,
      overviewError: new HttpError(403, "administrator access required"),
    });
    expect(
      await screen.findByText("An administrator keeps this server's data"),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Export…" })).toBeEnabled();
    for (const name of ["Back up…", "Reset settings…", "Try again"]) {
      expect(screen.queryByRole("button", { name })).not.toBeInTheDocument();
    }
    expect(
      screen.queryByText(/administrator access required/),
    ).not.toBeInTheDocument();
  });
});
