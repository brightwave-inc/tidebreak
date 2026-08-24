// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { AppSummary } from "@/api";
import { AppsView } from "./AppsView";
import type { AppsApis } from "./appsApis";

afterEach(cleanup);

const apps: AppSummary[] = [
  {
    id: "release-brief",
    name: "Release brief",
    revision_count: 4,
    updated_at: "2026-08-23T16:40:00.000Z",
    granted: true,
  },
  {
    id: "incident-map",
    name: "Incident map",
    revision_count: 1,
    updated_at: "2026-08-22T09:15:00.000Z",
    granted: false,
  },
];

function appsApis(rows: AppSummary[]): AppsApis {
  return {
    baseUrl: "",
    list: vi.fn().mockResolvedValue({ apps: rows }),
    get: vi.fn(),
    deleteApp: vi.fn(),
    grantState: vi.fn(),
    consent: vi.fn(),
    revoke: vi.fn(),
    viewSession: vi.fn(),
    invokeOperation: vi.fn(),
    invokeGatewayOperation: vi.fn(),
    invokeFolder: vi.fn(),
    gatewayBaseUrl: vi.fn(),
    gatewayPage: vi.fn(),
  };
}

describe("AppsView", () => {
  it("groups saved apps and keeps access state next to each app", async () => {
    render(<AppsView apis={appsApis(apps)} onOpen={() => {}} />);

    expect(
      await screen.findByRole("region", { name: "Saved apps" }),
    ).toBeVisible();
    expect(screen.getByRole("list", { name: "Apps" })).toBeVisible();
    expect(screen.getByText("4 revisions · Updated Aug 23")).toBeVisible();
    expect(screen.getByText("Access allowed")).toBeVisible();
  });

  it("shows a useful empty state", async () => {
    render(<AppsView apis={appsApis([])} onOpen={() => {}} />);

    expect(await screen.findByText("No apps yet")).toBeVisible();
    expect(
      screen.getByText(/stays here so you can open it again/i),
    ).toBeVisible();
  });

  it("shows one failure state when the initial request fails", async () => {
    const apis = appsApis([]);
    vi.mocked(apis.list).mockRejectedValueOnce(
      new Error("The app library did not answer."),
    );

    render(<AppsView apis={apis} onOpen={() => {}} />);

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Apps could not load",
    );
    expect(screen.getByRole("alert")).toHaveTextContent(
      "The app library did not answer.",
    );
    expect(screen.queryByText("No apps yet")).not.toBeInTheDocument();
  });

  it("keeps cached apps visible when a refresh fails", async () => {
    const apis = appsApis(apps);
    vi.mocked(apis.list)
      .mockResolvedValueOnce({ apps })
      .mockRejectedValueOnce(new Error("The app library did not answer."));

    render(<AppsView apis={apis} onOpen={() => {}} />);
    expect(
      await screen.findByRole("region", { name: "Saved apps" }),
    ).toBeVisible();

    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "The app library did not answer.",
    );
    expect(screen.getByRole("region", { name: "Saved apps" })).toBeVisible();
    expect(screen.queryByText("No apps yet")).not.toBeInTheDocument();
  });
});
