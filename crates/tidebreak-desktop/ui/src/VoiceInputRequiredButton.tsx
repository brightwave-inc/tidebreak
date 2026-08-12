import { useNavigate } from "@tanstack/react-router";
import { Button } from "@/components/ui/button";

export function VoiceInputRequiredButton() {
  const navigate = useNavigate();
  const settingsPath: string = "/settings/voice-transcription";
  return (
    <Button type="button" onClick={() => void navigate({ to: settingsPath })}>
      Configure voice input
    </Button>
  );
}
