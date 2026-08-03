// @vitest-environment jsdom
//
// Reproduction of a live defect: typing in the chat composer re-rendered the
// panel tree, and every re-render tore Univer down and remounted it, because
// the viewer's init effect depended on the object `useUniverWorker` rebuilds
// each render. Univer itself cannot run in jsdom, so its engine is mocked at
// the module boundary and the assertion is on the init path: `createUniver`
// must fire exactly once while a parent re-renders per keystroke and hands the
// viewer a fresh `source` object each time, the way the panels really do.
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { clearFileDownloadCache, type FileBytesSource } from "./useFileDownload";

const univerMocks = vi.hoisted(() => {
  const univer = { dispose: vi.fn() };
  const univerAPI = {
    toggleDarkMode: vi.fn(),
    createWorkbook: vi.fn(),
    addEvent: vi.fn(),
    Event: { BeforeCommandExecute: "BeforeCommandExecute" },
    getActiveWorkbook: () => null,
    dispose: vi.fn(),
  };
  return {
    univer,
    univerAPI,
    createUniver: vi.fn(() => ({ univer, univerAPI })),
  };
});

vi.mock("@univerjs/presets", () => ({
  createUniver: univerMocks.createUniver,
  LocaleType: { EN_US: "enUS" },
}));

vi.mock("@univerjs/preset-sheets-core", () => ({
  UniverSheetsCorePreset: vi.fn(() => ({})),
  CalculationMode: { WHEN_EMPTY: 0 },
  sequenceNodeType: { REFERENCE: 3 },
}));

vi.mock("@univerjs/preset-sheets-core/locales/en-US", () => ({ default: {} }));
vi.mock("@univerjs/preset-sheets-core/lib/index.css", () => ({}));
vi.mock("@univerjs/core", () => ({ ICommandService: class {} }));

vi.mock("@/workers/univer-formula.worker?worker&inline", () => ({
  default: class {},
}));

// A parser worker that answers every parse with an empty workbook, so the real
// `useUniverWorker` hook — the code under test — runs against it unchanged.
vi.mock("@/workers/univer-parser.worker?worker&inline", () => ({
  default: class FakeParserWorker {
    private listeners = new Set<(event: MessageEvent) => void>();

    addEventListener(type: string, listener: (event: MessageEvent) => void) {
      if (type === "message") this.listeners.add(listener);
    }

    removeEventListener(_type: string, listener: (event: MessageEvent) => void) {
      this.listeners.delete(listener);
    }

    postMessage(message: { type: string; opId?: string }) {
      if (message.type !== "parse") return;
      queueMicrotask(() => {
        const event = {
          data: {
            type: "result",
            opId: message.opId,
            workbookData: { id: "wb", sheetOrder: [], sheets: {} },
          },
        } as MessageEvent;
        for (const listener of this.listeners) listener(event);
      });
    }
  },
}));

import UniverSpreadsheetViewer from "./UniverSpreadsheetViewer";

const bytes = new TextEncoder().encode("a,b\n1,2\n");

/** A fresh source object per call, the way `OutputContent` builds one per render. */
function freshSource(): FileBytesSource {
  return {
    id: "out-1/rev-1",
    cacheKey: "output/chat-1/out-1/rev-1",
    fetch: async () => ({ bytes, contentType: "text/csv" }),
  };
}

/**
 * The shape of the real panel arrangement: composer state and the viewer under
 * one parent, so each keystroke re-renders the viewer with all-new props.
 */
function PaneWithViewer() {
  const [draft, setDraft] = useState("");
  return (
    <div>
      <textarea
        aria-label="Message"
        value={draft}
        onChange={(event) => setDraft(event.target.value)}
      />
      <UniverSpreadsheetViewer source={freshSource()} isCsv />
    </div>
  );
}

beforeEach(() => {
  clearFileDownloadCache();
  univerMocks.createUniver.mockClear();
  univerMocks.univer.dispose.mockClear();
  univerMocks.univerAPI.dispose.mockClear();
});
afterEach(cleanup);

it("keeps one Univer instance while the composer beside it is typed into", async () => {
  render(<PaneWithViewer />);
  await waitFor(() => expect(univerMocks.createUniver).toHaveBeenCalledTimes(1));

  const message = screen.getByLabelText("Message");
  await userEvent.type(message, "summarize the sheet");
  expect(message).toHaveValue("summarize the sheet");

  // Still the first instance: no keystroke disposed it or built another.
  expect(univerMocks.createUniver).toHaveBeenCalledTimes(1);
  expect(univerMocks.univer.dispose).not.toHaveBeenCalled();
});
