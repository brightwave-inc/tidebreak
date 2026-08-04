import { describe, expect, it, vi } from "vitest";

import {
  copyChatDebug,
  DEBUG_CONTENTS_NOTICE,
  saveChatDebug,
  type ChatDebugDeps,
  type DebugNotice,
} from "./ChatDebugBundle";
import { PICKER_BUSY_MESSAGE, PICKER_HOLDERS } from "./NativePickerLatch";

function deps(overrides: Partial<ChatDebugDeps> = {}) {
  const notices: DebugNotice[] = [];
  const released: string[] = [];
  const base: ChatDebugDeps = {
    buildBundle: vi.fn(async () => "# OpenWave chat debug bundle\n"),
    saveBundle: vi.fn(async () => true),
    writeClipboard: vi.fn(async () => {}),
    notify: (notice) => notices.push(notice),
    claimPicker: () => true,
    releasePicker: (holder) => released.push(holder),
    ...overrides,
  };
  return { deps: base, notices, released };
}

describe("chat debug bundle actions", () => {
  it("copies what the host rendered and says what is in it", async () => {
    const { deps: api, notices } = deps();
    await copyChatDebug("chat-1", api);

    expect(api.buildBundle).toHaveBeenCalledWith("chat-1");
    expect(api.writeClipboard).toHaveBeenCalledWith(
      "# OpenWave chat debug bundle\n",
    );
    expect(notices).toEqual([
      { message: "Debug info copied", description: DEBUG_CONTENTS_NOTICE },
    ]);
  });

  it("points at the file export when the clipboard write fails", async () => {
    const { deps: api, notices } = deps({
      writeClipboard: vi.fn(async () => {
        throw new Error("clipboard unavailable");
      }),
    });
    await copyChatDebug("chat-1", api);

    expect(notices[0]?.error).toBe(true);
    expect(notices[0]?.description).toContain("Save debug bundle");
  });

  it("releases the native picker whether the save lands, is dismissed, or fails", async () => {
    const { deps: saved, released } = deps();
    await saveChatDebug("chat-1", saved);
    expect(released).toEqual([PICKER_HOLDERS.saveDebugBundle]);

    // A dismissed dialog is not an outcome worth announcing.
    const { deps: dismissed, notices: quiet } = deps({
      saveBundle: vi.fn(async () => false),
    });
    await saveChatDebug("chat-1", dismissed);
    expect(quiet).toEqual([]);

    const {
      deps: failed,
      notices,
      released: releasedAfterFailure,
    } = deps({
      saveBundle: vi.fn(async () => {
        throw new Error("write failed");
      }),
    });
    await saveChatDebug("chat-1", failed);
    expect(notices[0]?.error).toBe(true);
    expect(releasedAfterFailure).toEqual([PICKER_HOLDERS.saveDebugBundle]);
  });

  it("refuses a second native picker instead of racing the host", async () => {
    const { deps: api, notices, released } = deps({ claimPicker: () => false });
    await saveChatDebug("chat-1", api);

    expect(api.saveBundle).not.toHaveBeenCalled();
    expect(notices).toEqual([{ message: PICKER_BUSY_MESSAGE, error: true }]);
    expect(released).toEqual([]);
  });
});
