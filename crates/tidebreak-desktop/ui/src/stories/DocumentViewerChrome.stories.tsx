import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";
import { FileDownloadProgressIndicator } from "@/components/document/FileDownloadProgress";
import {
  DocumentDetailActions,
  DocumentDetailBreadcrumb,
} from "@/document-detail/DocumentDetailHeader";
import { SpreadsheetShortcutsInfoBar } from "@/document/SpreadsheetShortcutsInfo";

type ChromeVariant = "pdf" | "spreadsheet" | "download" | "shared";

function PdfChrome() {
  const page = 7;
  return (
    <>
      <header className="flex h-11 shrink-0 items-center justify-between gap-3 border-b px-3">
        <DocumentDetailBreadcrumb documentName="Board packet.pdf" />
        <div className="flex min-w-0 items-center gap-2">
          <p className="text-xs tabular-nums text-muted-foreground">
            {page} / 24
          </p>
          <DocumentDetailActions
            canDownload
            onDownload={fn()}
            onAddToProject={fn()}
          />
        </div>
      </header>
      <div className="min-h-0 grow overflow-auto bg-muted/35 p-6">
        <div className="mx-auto flex max-w-3xl flex-col gap-5">
          {[page, page + 1].map((pageNumber) => (
            <article
              key={pageNumber}
              className="aspect-[8.5/11] rounded-sm border bg-background p-10 shadow-xs"
            >
              <p className="text-xs font-medium text-muted-foreground">
                Board packet · Page {pageNumber}
              </p>
              <h2 className="mt-8 text-2xl font-semibold tracking-tight">
                Renewal health and account actions
              </h2>
              <div className="mt-8 grid grid-cols-3 gap-3">
                {["$4.8M", "62%", "18 days"].map((value) => (
                  <div key={value} className="rounded-md bg-muted/50 p-4">
                    <p className="text-xl font-semibold tabular-nums">
                      {value}
                    </p>
                    <p className="mt-1 text-xs text-muted-foreground">
                      Operating signal
                    </p>
                  </div>
                ))}
              </div>
            </article>
          ))}
        </div>
      </div>
    </>
  );
}

function DocumentViewerChromeStory({ variant }: { variant: ChromeVariant }) {
  return (
    <div className="flex h-screen min-h-0 flex-col overflow-hidden bg-page-background text-foreground">
      {variant === "pdf" ? (
        <PdfChrome />
      ) : variant === "download" ? (
        <>
          <header className="flex h-11 shrink-0 items-center justify-between border-b px-3">
            <DocumentDetailBreadcrumb documentName="Research archive.pdf" />
          </header>
          <FileDownloadProgressIndicator
            progress={{
              loaded: 12.6 * 1024 * 1024,
              total: 28.4 * 1024 * 1024,
              percentage: 44.4,
            }}
            className="grow"
          />
        </>
      ) : (
        <>
          <header className="flex h-11 shrink-0 items-center justify-between gap-3 border-b px-3">
            <DocumentDetailBreadcrumb documentName="Account forecast.xlsx" />
            <DocumentDetailActions
              canDownload
              canAddToProject
              shared={variant === "shared"}
              onDownload={fn()}
              onAddToProject={fn()}
            />
          </header>
          <SpreadsheetShortcutsInfoBar onAutofit={fn()} />
          <div className="min-h-0 grow overflow-auto bg-background p-4 font-mono text-xs">
            <div className="min-w-[760px] overflow-hidden rounded-md border">
              <div className="grid grid-cols-[3rem_12rem_repeat(4,8rem)] bg-muted/60 font-medium">
                {["", "Account", "ARR", "Renewal", "Risk", "Owner"].map(
                  (cell) => (
                    <div key={cell || "corner"} className="border-r px-2 py-2">
                      {cell}
                    </div>
                  ),
                )}
              </div>
              {Array.from({ length: 12 }, (_, index) => (
                <div
                  key={index}
                  className="grid grid-cols-[3rem_12rem_repeat(4,8rem)] border-t"
                >
                  <div className="bg-muted/35 px-2 py-2 text-muted-foreground">
                    {index + 1}
                  </div>
                  <div className="border-l px-2 py-2">Account {index + 1}</div>
                  <div className="border-l px-2 py-2 tabular-nums">
                    ${(428 - index * 17).toLocaleString()}K
                  </div>
                  <div className="border-l px-2 py-2">2026-10-{15 + index}</div>
                  <div className="border-l px-2 py-2">
                    {index % 3 === 0 ? "High" : "Medium"}
                  </div>
                  <div className="border-l px-2 py-2">CS {index + 1}</div>
                </div>
              ))}
            </div>
          </div>
        </>
      )}
    </div>
  );
}

const meta = {
  title: "Documents/Viewer chrome",
  component: DocumentViewerChromeStory,
  parameters: { layout: "fullscreen" },
  args: { variant: "pdf" },
} satisfies Meta<typeof DocumentViewerChromeStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const PdfHeader: Story = {};

export const SpreadsheetHeader: Story = {
  args: { variant: "spreadsheet" },
};

export const DownloadProgress: Story = {
  args: { variant: "download" },
};

export const AddedToProject: Story = {
  args: { variant: "shared" },
};

export const CompactPdfHeader: Story = {
  globals: { viewport: { value: "compact", isRotated: false } },
};

export const CompactDownloadProgress: Story = {
  args: { variant: "download" },
  globals: { viewport: { value: "compact", isRotated: false } },
};

export const CompactSpreadsheetHeader: Story = {
  args: { variant: "spreadsheet" },
  globals: { viewport: { value: "compact", isRotated: false } },
};
