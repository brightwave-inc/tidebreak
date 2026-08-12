import { describe, expect, it } from "vitest";

import type { LocalVoiceModelInfo, VoiceTranscriptionInfo } from "./api";
import { voiceSelectionReady } from "./VoiceInputStore";
import { voiceSelectionValue } from "./settings/VoiceTranscriptionPanel";

function model(
  id: string,
  state: LocalVoiceModelInfo["state"],
): LocalVoiceModelInfo {
  return {
    id,
    label: id,
    description: "",
    total_bytes: 1_000,
    english_only: true,
    recommended: false,
    state,
    downloaded_bytes: null,
    error: null,
  };
}

function info(overrides: Partial<VoiceTranscriptionInfo> = {}): VoiceTranscriptionInfo {
  return {
    model: "local",
    local_model: "tiny.en-q5_1",
    local_models: [model("tiny.en-q5_1", "ready"), model("small.en-q5_1", "not_installed")],
    openai_ready: false,
    gemini_ready: false,
    ...overrides,
  };
}

describe("voice input selection", () => {
  it("is ready only when the selected local model is the installed one", () => {
    expect(voiceSelectionReady(info())).toBe(true);
    // Selecting a catalog entry that has not been downloaded must still send
    // the mic to settings rather than start a recording nothing can transcribe.
    expect(voiceSelectionReady(info({ local_model: "small.en-q5_1" }))).toBe(false);
    expect(voiceSelectionReady(info({ local_model: "removed-from-catalog" }))).toBe(
      false,
    );
  });

  it("keeps the local model out of the provider choice", () => {
    expect(voiceSelectionValue(info({ local_model: "small.en-q5_1" }))).toBe(
      "local:small.en-q5_1",
    );
    expect(voiceSelectionValue(info({ model: "gemini_flash" }))).toBe("gemini_flash");
  });
});
