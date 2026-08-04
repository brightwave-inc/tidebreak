import { useCallback, useEffect, useRef, useState } from "react";

export type VoiceComposerState =
  | "idle"
  | "requesting"
  | "recording"
  | "transcribing";

export type VoiceTranscriber = (audio: Blob) => Promise<string>;

export type VoiceComposer = {
  available: boolean;
  state: VoiceComposerState;
  error: string | null;
  start: () => Promise<void>;
  stop: () => void;
};

const VOICE_MIME_TYPES = [
  "audio/webm;codecs=opus",
  "audio/webm",
  "audio/mp4",
] as const;
const MIN_RECORDING_MS = 250;

export function supportedVoiceMimeType(): string | undefined {
  return VOICE_MIME_TYPES.find((type) => MediaRecorder.isTypeSupported(type));
}

export function appendTranscript(draft: string, transcript: string): string {
  const spoken = transcript.trim();
  if (!spoken) return draft;
  if (!draft) return spoken;
  return `${draft}${/\s$/.test(draft) ? "" : " "}${spoken}`;
}

function voiceError(error: unknown): string {
  if (error instanceof DOMException && error.name === "NotAllowedError") {
    return "Microphone access was denied. Allow it in system settings and try again.";
  }
  const message = String(error).replace(/^Error:\s*/, "").trim();
  return message || "Could not record from the microphone.";
}

/**
 * The browser-owned half of voice composition: ask for the microphone only
 * after a click, capture one bounded recording, and hand its bytes to the
 * product-selected transcriber. The hook never chooses or contacts a provider.
 */
export function useVoiceComposer(
  transcribe: VoiceTranscriber,
  onTranscript: (transcript: string) => void,
  now: () => number = () => performance.now(),
): VoiceComposer {
  const available =
    typeof navigator !== "undefined" &&
    typeof MediaRecorder !== "undefined" &&
    typeof navigator.mediaDevices?.getUserMedia === "function";
  const [state, setState] = useState<VoiceComposerState>("idle");
  const [error, setError] = useState<string | null>(null);
  const recorderRef = useRef<MediaRecorder | null>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const chunksRef = useRef<Blob[]>([]);
  const startedAtRef = useRef(0);
  const recorderFailedRef = useRef(false);
  const mountedRef = useRef(true);
  const transcribeRef = useRef(transcribe);
  const onTranscriptRef = useRef(onTranscript);
  transcribeRef.current = transcribe;
  onTranscriptRef.current = onTranscript;

  const releaseStream = useCallback(() => {
    for (const track of streamRef.current?.getTracks() ?? []) track.stop();
    streamRef.current = null;
    recorderRef.current = null;
  }, []);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      const recorder = recorderRef.current;
      if (recorder?.state === "recording") recorder.stop();
      releaseStream();
    };
  }, [releaseStream]);

  const start = useCallback(async () => {
    if (!available || recorderRef.current) return;
    setError(null);
    setState("requesting");
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      if (!mountedRef.current) {
        for (const track of stream.getTracks()) track.stop();
        return;
      }
      const mimeType = supportedVoiceMimeType();
      const recorder = new MediaRecorder(
        stream,
        mimeType ? { mimeType } : undefined,
      );
      streamRef.current = stream;
      recorderRef.current = recorder;
      chunksRef.current = [];
      recorderFailedRef.current = false;
      recorder.addEventListener("dataavailable", (event) => {
        if (event.data.size > 0) chunksRef.current.push(event.data);
      });
      recorder.addEventListener("stop", () => {
        const elapsed = now() - startedAtRef.current;
        const audio = new Blob(chunksRef.current, {
          type: recorder.mimeType || chunksRef.current[0]?.type,
        });
        chunksRef.current = [];
        releaseStream();
        if (!mountedRef.current) return;
        if (recorderFailedRef.current) return;
        if (audio.size === 0 || elapsed < MIN_RECORDING_MS) {
          setState("idle");
          setError("That recording was too short. Hold the button a little longer and try again.");
          return;
        }
        setState("transcribing");
        void transcribeRef.current(audio)
          .then((transcript) => {
            if (!mountedRef.current) return;
            if (transcript.trim()) onTranscriptRef.current(transcript);
          })
          .catch((caught) => {
            if (mountedRef.current) setError(voiceError(caught));
          })
          .finally(() => {
            if (mountedRef.current) setState("idle");
          });
      });
      recorder.addEventListener("error", (event) => {
        recorderFailedRef.current = true;
        chunksRef.current = [];
        releaseStream();
        if (!mountedRef.current) return;
        setState("idle");
        setError(voiceError((event as ErrorEvent).error ?? "The recording failed."));
      });
      startedAtRef.current = now();
      recorder.start();
      setState("recording");
    } catch (caught) {
      releaseStream();
      if (!mountedRef.current) return;
      setState("idle");
      setError(voiceError(caught));
    }
  }, [available, now, releaseStream]);

  const stop = useCallback(() => {
    const recorder = recorderRef.current;
    if (recorder?.state === "recording") recorder.stop();
  }, []);

  return { available, state, error, start, stop };
}
