// @vitest-environment jsdom
import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  appendTranscript,
  supportedVoiceMimeType,
  useVoiceComposer,
} from "./useVoiceComposer";

class FakeMediaRecorder extends EventTarget {
  static latest: FakeMediaRecorder | null = null;
  state: RecordingState = "inactive";
  mimeType = "audio/webm";

  static isTypeSupported = vi.fn((type: string) => type === "audio/webm");
  static chunk = new Blob(["voice"], { type: "audio/webm" });
  options: MediaRecorderOptions | undefined;

  constructor(_stream: MediaStream, options?: MediaRecorderOptions) {
    super();
    this.options = options;
    FakeMediaRecorder.latest = this;
  }

  start() {
    this.state = "recording";
  }

  stop() {
    this.state = "inactive";
    this.dispatchEvent(
      new MessageEvent("dataavailable", {
        data: FakeMediaRecorder.chunk,
      }),
    );
    this.dispatchEvent(new Event("stop"));
  }
}

describe("voice composer", () => {
  beforeEach(() => {
    FakeMediaRecorder.latest = null;
    FakeMediaRecorder.chunk = new Blob(["voice"], { type: "audio/webm" });
    FakeMediaRecorder.isTypeSupported.mockClear();
    vi.stubGlobal("MediaRecorder", FakeMediaRecorder);
  });

  it("prefers opus, then webm, then mp4 when the webview supports them", () => {
    expect(supportedVoiceMimeType()).toBe("audio/webm");
    expect(FakeMediaRecorder.isTypeSupported).toHaveBeenNthCalledWith(
      1,
      "audio/webm;codecs=opus",
    );
    expect(FakeMediaRecorder.isTypeSupported).toHaveBeenNthCalledWith(
      2,
      "audio/webm",
    );
  });

  it("records, exposes transcription state, and appends the editable result", async () => {
    const stopTrack = vi.fn();
    const getUserMedia = vi.fn(async () => ({
      getTracks: () => [{ stop: stopTrack }],
    }));
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: { getUserMedia },
    });
    let resolveTranscript!: (value: string) => void;
    const transcribe = vi.fn(
      () =>
        new Promise<string>((resolve) => {
          resolveTranscript = resolve;
        }),
    );
    let draft = "Existing draft";
    const { result } = renderHook(() =>
      useVoiceComposer(
        transcribe,
        (transcript) => {
          draft = appendTranscript(draft, transcript);
        },
        vi.fn().mockReturnValueOnce(0).mockReturnValue(500),
      ),
    );

    await act(async () => result.current.start());
    expect(result.current.state).toBe("recording");
    expect(FakeMediaRecorder.latest?.options).toEqual({ mimeType: "audio/webm" });
    expect(getUserMedia).toHaveBeenCalledWith({ audio: true });

    act(() => result.current.stop());
    expect(result.current.state).toBe("transcribing");
    expect(stopTrack).toHaveBeenCalledOnce();
    expect(transcribe).toHaveBeenCalledWith(expect.any(Blob));

    resolveTranscript(" spoken words ");
    await waitFor(() => expect(result.current.state).toBe("idle"));
    expect(draft).toBe("Existing draft spoken words");
  });

  it("rejects an empty or too-short recording before transcription", async () => {
    FakeMediaRecorder.chunk = new Blob([]);
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: {
        getUserMedia: vi.fn(async () => ({ getTracks: () => [] })),
      },
    });
    const transcribe = vi.fn(async () => "ignored");
    const { result } = renderHook(() =>
      useVoiceComposer(
        transcribe,
        vi.fn(),
        vi.fn().mockReturnValueOnce(0).mockReturnValue(100),
      ),
    );

    await act(async () => result.current.start());
    act(() => result.current.stop());

    expect(result.current.state).toBe("idle");
    expect(result.current.error).toContain("too short");
    expect(transcribe).not.toHaveBeenCalled();
  });

  it("cleans up the stream and reports a recorder failure", async () => {
    const stopTrack = vi.fn();
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: {
        getUserMedia: vi.fn(async () => ({
          getTracks: () => [{ stop: stopTrack }],
        })),
      },
    });
    const transcribe = vi.fn(async () => "ignored");
    const { result } = renderHook(() =>
      useVoiceComposer(transcribe, vi.fn(), () => 0),
    );

    await act(async () => result.current.start());
    act(() => {
      FakeMediaRecorder.latest?.dispatchEvent(
        new ErrorEvent("error", { error: new Error("Recorder crashed") }),
      );
    });

    expect(result.current.state).toBe("idle");
    expect(result.current.error).toBe("Recorder crashed");
    expect(stopTrack).toHaveBeenCalledOnce();
    expect(transcribe).not.toHaveBeenCalled();
  });

  it("keeps provider failure visible without changing the draft", async () => {
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: {
        getUserMedia: vi.fn(async () => ({ getTracks: () => [] })),
      },
    });
    const onTranscript = vi.fn();
    const { result } = renderHook(() =>
      useVoiceComposer(
        vi.fn(async () => {
          throw new Error("Voice transcription is not configured yet.");
        }),
        onTranscript,
        vi.fn().mockReturnValueOnce(0).mockReturnValue(500),
      ),
    );

    await act(async () => result.current.start());
    act(() => result.current.stop());

    await waitFor(() =>
      expect(result.current.error).toBe(
        "Voice transcription is not configured yet.",
      ),
    );
    expect(onTranscript).not.toHaveBeenCalled();
  });
});
