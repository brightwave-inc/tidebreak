// @vitest-environment jsdom
import {
  cleanup,
  createEvent,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

import { Composer, type ComposerFiles, type ComposerImages } from "./Composer";

afterEach(cleanup);

const noop = async () => undefined;

function images(overrides: Partial<ComposerImages> = {}): ComposerImages {
  return {
    items: [],
    error: null,
    unsupportedModel: null,
    onAttachFiles: vi.fn(),
    onRemove: vi.fn(),
    onRetry: vi.fn(),
    ...overrides,
  };
}

function files(overrides: Partial<ComposerFiles> = {}): ComposerFiles {
  return {
    items: [],
    attaching: false,
    onAttach: vi.fn(),
    onRemove: vi.fn(),
    ...overrides,
  };
}

function markdownFile(type = "text/markdown"): File {
  return new File(["# Notes\n"], "notes.md", { type });
}

function csvFile(type = "text/csv"): File {
  return new File(["a,b\n1,2\n"], "data.csv", { type });
}

function renderComposer(
  overrides: { images?: ComposerImages; files?: ComposerFiles } = {},
) {
  return render(
    <Composer
      activeTurnId={null}
      busy={false}
      cancelError={null}
      cancelPending={false}
      disabled={false}
      draft=""
      images={overrides.images ?? images()}
      files={overrides.files ?? files()}
      onDraftChange={vi.fn()}
      onSend={noop}
      onSteer={noop}
      onStop={noop}
      resetKey="chat-1"
      steerError={null}
      steerPending={false}
      steerStatus={null}
    />,
  );
}

function dropFiles(dropped: File[]) {
  const form = document.querySelector("form.chat-composer");
  if (!(form instanceof HTMLFormElement)) {
    throw new Error("expected the composer form");
  }
  const event = createEvent.drop(form, {
    dataTransfer: { files: dropped, types: ["Files"] },
  });
  fireEvent(form, event);
  return event;
}

function pasteFiles(target: HTMLElement, pasted: File[]) {
  const event = createEvent.paste(target, {
    clipboardData: {
      files: pasted,
      getData: () => "",
    },
  });
  fireEvent(target, event);
  return event;
}

it("routes dropped markdown and csv through document attach, not the image validator", () => {
  const onAttachHeld = vi.fn();
  const onAttachFiles = vi.fn();
  renderComposer({
    images: images({ onAttachFiles }),
    files: files({ onAttachHeld }),
  });

  dropFiles([markdownFile(), csvFile()]);

  expect(onAttachHeld).toHaveBeenCalledOnce();
  expect(onAttachHeld.mock.calls[0][0].map((file: File) => file.name)).toEqual([
    "notes.md",
    "data.csv",
  ]);
  expect(onAttachFiles).not.toHaveBeenCalled();
});

it("does not reject markdown or csv whose browser type looks like an unsupported image", () => {
  const onAttachHeld = vi.fn();
  const onAttachFiles = vi.fn();
  renderComposer({
    images: images({ onAttachFiles }),
    files: files({ onAttachHeld }),
  });

  dropFiles([markdownFile("image/heic"), csvFile("image/tiff")]);

  expect(onAttachHeld).toHaveBeenCalledOnce();
  expect(onAttachHeld.mock.calls[0][0].map((file: File) => file.name)).toEqual([
    "notes.md",
    "data.csv",
  ]);
  expect(onAttachFiles).not.toHaveBeenCalled();
});

it("pastes markdown and csv through the same mixed attach path", () => {
  const onAttachHeld = vi.fn();
  const onAttachFiles = vi.fn();
  renderComposer({
    images: images({ onAttachFiles }),
    files: files({ onAttachHeld }),
  });

  const event = pasteFiles(screen.getByRole("textbox", { name: "Message" }), [
    markdownFile(),
    csvFile(),
  ]);

  expect(event.defaultPrevented).toBe(true);
  expect(onAttachHeld).toHaveBeenCalledOnce();
  expect(onAttachHeld.mock.calls[0][0].map((file: File) => file.name)).toEqual([
    "notes.md",
    "data.csv",
  ]);
  expect(onAttachFiles).not.toHaveBeenCalled();
});

it("keeps image-only transfer when the surface has no document ingest", () => {
  const onAttachFiles = vi.fn();
  renderComposer({
    images: images({ onAttachFiles }),
    files: files(),
  });

  dropFiles([markdownFile("image/heic"), csvFile()]);

  expect(onAttachFiles).toHaveBeenCalledOnce();
  expect(onAttachFiles.mock.calls[0][0].map((file: File) => file.name)).toEqual(
    ["notes.md"],
  );
});
