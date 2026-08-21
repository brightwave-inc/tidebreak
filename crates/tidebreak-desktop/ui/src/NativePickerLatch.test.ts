import { beforeEach, describe, expect, it } from "vitest";
import {
  PICKER_HOLDERS,
  createNativePickerLatchStore,
  pickerHeldByAnotherSurface,
  useNativePickerLatch,
} from "./NativePickerLatch";

describe("native picker latch", () => {
  beforeEach(() => {
    useNativePickerLatch.setState({ holder: null });
  });

  it("admits one holder at a time regardless of which surface asks", () => {
    const latch = createNativePickerLatchStore();
    expect(latch.getState().claim(PICKER_HOLDERS.connectFolder)).toBe(true);
    // The host serialises every picker behind one mutex, so a *different*
    // surface asking is exactly the collision this exists to prevent — that is
    // the case a per-surface latch missed.
    expect(latch.getState().claim(PICKER_HOLDERS.importSource)).toBe(false);
    expect(latch.getState().claim("some-folder-access-call-id")).toBe(false);
    expect(latch.getState().claim(PICKER_HOLDERS.exportOutput)).toBe(false);

    latch.getState().release(PICKER_HOLDERS.connectFolder);
    expect(latch.getState().claim(PICKER_HOLDERS.importSource)).toBe(true);
  });

  it("ignores a release from a surface that never held it", () => {
    const latch = createNativePickerLatchStore();
    expect(latch.getState().claim(PICKER_HOLDERS.connectFolder)).toBe(true);
    // A losing caller runs its own cleanup; that must not free the picker
    // out from under the surface actually showing one.
    latch.getState().release(PICKER_HOLDERS.importSource);
    expect(latch.getState().holder).toBe(PICKER_HOLDERS.connectFolder);
    expect(latch.getState().claim(PICKER_HOLDERS.exportOutput)).toBe(false);
  });

  it("lets the holding surface tell itself apart from the blocked ones", () => {
    expect(
      pickerHeldByAnotherSurface(
        PICKER_HOLDERS.connectFolder,
        PICKER_HOLDERS.connectFolder,
      ),
    ).toBe(false);
    expect(
      pickerHeldByAnotherSurface(
        PICKER_HOLDERS.connectFolder,
        PICKER_HOLDERS.importSource,
      ),
    ).toBe(true);
    expect(pickerHeldByAnotherSurface(null, PICKER_HOLDERS.importSource)).toBe(
      false,
    );
  });

  it("names every surface that opens a host picker", () => {
    // Eight Tauri commands take the host picker mutex: resolving a
    // folder-access decision (keyed by call id), connecting a folder,
    // confirming a previously approved one, granting a capability on an
    // attached folder, importing a source, exporting an output, saving a chat
    // debug bundle, and attaching an image to a message.
    expect(Object.values(PICKER_HOLDERS)).toEqual([
      "connect-folder",
      "confirm-approved-folder",
      "grant-folder-capability",
      "import-source",
      "export-output",
      "save-debug-bundle",
      "attach-image",
    ]);
  });
});
